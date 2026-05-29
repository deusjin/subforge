//! Translation quality pipeline: MAPS terminology → GEMBA-MQM scoring → Targeted Refine

use super::llm::call_chat_with_system;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// --- Terminology (MAPS) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermEntry {
    pub term: String,
    pub action: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub context: String,
}

/// Extract key terms from source text for consistent translation
pub async fn extract_keywords(
    texts: &[String],
    target_lang: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Result<Vec<TermEntry>, String> {
    // Stride-sample across the whole video so terminology introduced later isn't missed.
    // Targets ~50 lines spread across the full timeline.
    let combined = stride_sample(texts, 50).join("\n");
    let system = format!(
        "You are a terminology extractor for subtitle translation (target: {target_lang}).\n\
        Extract important terms. For each:\n\
        - \"keep\": code identifiers, API names (e.g. getDamage, FString)\n\
        - \"translate\": domain terms with consistent translation (e.g. debuff → 减益效果)\n\
        - \"flexible\": terms where context determines translation\n\n\
        Output JSON array: [{{\"term\":\"...\",\"action\":\"keep|translate|flexible\",\"target\":\"...\",\"context\":\"...\"}}]\n\
        Max 30 entries. ONLY the JSON array."
    );
    let response = call_chat_with_system(api_key, base_url, model, &system, &combined).await?;
    parse_term_response(&response).ok_or_else(|| "failed to parse keywords".into())
}

/// Pick `n` lines evenly spaced across the input. Returns the original lines if shorter than n.
pub(crate) fn stride_sample(texts: &[String], n: usize) -> Vec<String> {
    if texts.len() <= n {
        return texts.to_vec();
    }
    let stride = texts.len() as f64 / n as f64;
    (0..n)
        .map(|i| {
            let idx = (i as f64 * stride) as usize;
            texts[idx.min(texts.len() - 1)].clone()
        })
        .collect()
}

fn parse_term_response(raw: &str) -> Option<Vec<TermEntry>> {
    if let Ok(entries) = serde_json::from_str::<Vec<TermEntry>>(raw) {
        return Some(entries);
    }
    extract_json_str(raw).and_then(|s| serde_json::from_str(&s).ok())
}

/// Format terms for injection into translation prompt
pub fn format_terms_for_prompt(terms: &[TermEntry]) -> String {
    if terms.is_empty() {
        return String::new();
    }
    let mut lines = vec!["Terminology (use flexibly based on context):".to_string()];
    for t in terms {
        match t.action.as_str() {
            "keep" => lines.push(format!("- \"{}\" → keep as-is", t.term)),
            "translate" => lines.push(format!("- \"{}\" → \"{}\"", t.term, t.target)),
            _ => lines.push(format!("- \"{}\" → decide by context", t.term)),
        }
    }
    lines.join("\n")
}

// --- GEMBA-MQM Quality Estimation ---

#[derive(Debug, Clone)]
pub struct QeScore {
    pub index: usize,
    pub score: f64,
}

/// Outcome of a GEMBA run that distinguishes "evaluated and got a score" from
/// "evaluation failed (network/parse) — score unknown".
#[derive(Debug, Default)]
pub struct GembaResult {
    pub scores: Vec<QeScore>,
    /// Indices for which the evaluation request failed entirely.
    /// Caller should NOT assume these passed; they should be excluded from
    /// "low-score" buckets and refine.
    pub failed_indices: Vec<usize>,
}

/// Evaluate translation quality using GEMBA-MQM framework.
/// Returns scores for each (source, translation) pair.
pub async fn evaluate_gemba(
    sources: &[String],
    translations: &[String],
    target_lang: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
    concurrency: usize,
) -> GembaResult {
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();

    // Evaluate in batches of 10
    for chunk_start in (0..sources.len()).step_by(10) {
        let chunk_end = (chunk_start + 10).min(sources.len());
        let pairs: Vec<Value> = (chunk_start..chunk_end)
            .map(|i| json!({"i": i + 1, "src": &sources[i], "tgt": &translations[i]}))
            .collect();
        let chunk_indices: Vec<usize> = (chunk_start..chunk_end).collect();

        let sem = semaphore.clone();
        let api_key = api_key.to_string();
        let base_url = base_url.to_string();
        let model = model.to_string();
        let target_lang = target_lang.to_string();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            let system = format!(
                "You are a translation quality evaluator (MQM, target: {target_lang}).\n\
                Score each translation 0-100. Deduct: Critical=-25, Major=-5, Minor=-1.\n\
                Input: JSON array of {{i, src, tgt}}.\n\
                Output: JSON array of {{\"i\":N,\"score\":N}}. Nothing else."
            );
            let input = serde_json::to_string(&pairs).unwrap_or_default();
            match call_chat_with_system(&api_key, &base_url, &model, &system, &input).await {
                Ok(resp) => {
                    let parsed = parse_qe_response(&resp);
                    if parsed.is_empty() {
                        // Got a response but couldn't parse it — treat as full-batch failure
                        Err(chunk_indices)
                    } else {
                        Ok(parsed)
                    }
                }
                Err(_) => Err(chunk_indices),
            }
        }));
    }

    let mut all_scores = Vec::new();
    let mut failed_indices = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Ok(scores)) => all_scores.extend(scores),
            Ok(Err(indices)) => failed_indices.extend(indices),
            Err(_) => {
                // tokio task itself panicked — uncommon, but treat conservatively
            }
        }
    }
    all_scores.sort_by_key(|s| s.index);
    failed_indices.sort();
    GembaResult {
        scores: all_scores,
        failed_indices,
    }
}

fn parse_qe_response(raw: &str) -> Vec<QeScore> {
    let parse = |s: &str| -> Vec<QeScore> {
        serde_json::from_str::<Vec<Value>>(s)
            .unwrap_or_default()
            .iter()
            .filter_map(|item| {
                let i = item["i"].as_u64()? as usize;
                let score = item["score"].as_f64()?;
                Some(QeScore {
                    index: i - 1,
                    score,
                })
            })
            .collect()
    };

    let result = parse(raw);
    if !result.is_empty() {
        return result;
    }
    extract_json_str(raw).map(|s| parse(&s)).unwrap_or_default()
}

// --- Targeted Refine ---

/// Refine translations that scored below threshold.
/// Mutates `translations` in place, returns count of refined items.
#[allow(clippy::too_many_arguments)]
pub async fn refine_low_scores(
    sources: &[String],
    translations: &mut [String],
    scores: &[QeScore],
    threshold: f64,
    target_lang: &str,
    terms_prompt: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
    concurrency: usize,
) -> usize {
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    let low: Vec<&QeScore> = scores.iter().filter(|s| s.score < threshold).collect();
    if low.is_empty() {
        return 0;
    }

    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut handles = Vec::new();

    for qs in &low {
        let idx = qs.index;
        if idx >= sources.len() {
            continue;
        }
        let source = sources[idx].clone();
        let draft = translations[idx].clone();
        let sem = semaphore.clone();
        let api_key = api_key.to_string();
        let base_url = base_url.to_string();
        let model = model.to_string();
        let target_lang = target_lang.to_string();
        let terms_prompt = terms_prompt.to_string();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            let system = format!(
                "Improve this subtitle translation (target: {target_lang}).\n\
                 The previous translation scored poorly on quality.\n\
                 {terms_prompt}\n\
                 Return ONLY the improved translation."
            );
            let user = format!("Source: {source}\nDraft: {draft}");
            let result = call_chat_with_system(&api_key, &base_url, &model, &system, &user).await;
            (
                idx,
                result.ok().map(|s| s.trim().trim_matches('"').to_string()),
            )
        }));
    }

    let mut refined_count = 0;
    for handle in handles {
        if let Ok((idx, Some(improved))) = handle.await
            && !improved.is_empty()
            && idx < translations.len()
        {
            translations[idx] = improved;
            refined_count += 1;
        }
    }
    refined_count
}

// --- Glossary persistence ---

/// Maximum entries to keep in glossary.jsonl. Older entries are evicted on
/// save when the cap is exceeded. Mirrors `tm::MAX_MEMORY_ENTRIES` so
/// long-lived shared `tm_dir`s don't grow unboundedly.
pub const MAX_GLOSSARY_ENTRIES: usize = 2000;

pub fn save_glossary(terms: &[TermEntry], tm_dir: &std::path::Path) {
    if terms.is_empty() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(tm_dir) {
        crate::log_warn!("glossary save: create_dir failed: {e}");
        return;
    }

    // Hold the same advisory lock memory.jsonl uses, so concurrent writers
    // can't race on the read-merge-write cycle.
    let _lock = match super::tm::lock_tm_dir(tm_dir) {
        Ok(g) => g,
        Err(e) => {
            crate::log_warn!("glossary save: lock failed: {e}");
            return;
        }
    };

    let path = tm_dir.join("glossary.jsonl");
    let tmp = tm_dir.join("glossary.jsonl.tmp");

    let mut existing: Vec<TermEntry> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    for new in terms {
        if let Some(pos) = existing.iter().position(|e| e.term == new.term) {
            existing[pos] = new.clone();
        } else {
            existing.push(new.clone());
        }
    }

    // Cap to MAX_GLOSSARY_ENTRIES, dropping the oldest by file order.
    // Newly-pushed terms above are at the tail and survive eviction.
    if existing.len() > MAX_GLOSSARY_ENTRIES {
        let drop = existing.len() - MAX_GLOSSARY_ENTRIES;
        existing.drain(..drop);
    }

    let content: String = existing
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect::<Vec<_>>()
        .join("\n");
    if let Err(e) = std::fs::write(&tmp, content) {
        crate::log_warn!("glossary save: write tmp failed: {e}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        crate::log_warn!("glossary save: rename failed: {e}");
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn load_glossary(tm_dir: &std::path::Path) -> Vec<TermEntry> {
    let path = tm_dir.join("glossary.jsonl");
    std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// --- Helpers ---

fn extract_json_str(text: &str) -> Option<String> {
    // Try code block first
    for marker in ["```json\n", "```\n"] {
        if let Some(start) = text.find(marker) {
            let cs = start + marker.len();
            if let Some(end) = text[cs..].find("```") {
                return Some(text[cs..cs + end].trim().to_string());
            }
        }
    }
    // Try first [ ... ]
    let start = text.find('[')?;
    let bytes = &text.as_bytes()[start..];
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if esc {
            esc = false;
            continue;
        }
        match b {
            b'\\' if in_str => esc = true,
            b'"' => in_str = !in_str,
            b'[' if !in_str => depth += 1,
            b']' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..start + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stride_sample_returns_all_when_shorter() {
        let texts = vec!["a".to_string(), "b".to_string()];
        let sample = stride_sample(&texts, 5);
        assert_eq!(sample, vec!["a", "b"]);
    }

    #[test]
    fn stride_sample_covers_full_range() {
        let texts: Vec<String> = (0..100).map(|i| format!("line{i}")).collect();
        let sample = stride_sample(&texts, 10);
        assert_eq!(sample.len(), 10);
        assert_eq!(sample[0], "line0");
        // Last sample should be near the end (within ~10% of total length)
        let last_idx: usize = sample[9].trim_start_matches("line").parse().unwrap();
        assert!(last_idx >= 90, "last sample {last_idx} should be near end");
    }

    #[test]
    fn stride_sample_evenly_spaced() {
        let texts: Vec<String> = (0..50).map(|i| format!("{i}")).collect();
        let sample = stride_sample(&texts, 5);
        assert_eq!(sample.len(), 5);
        // Indices should be roughly 0, 10, 20, 30, 40
        let indices: Vec<usize> = sample.iter().map(|s| s.parse().unwrap()).collect();
        for i in 1..indices.len() {
            assert!(indices[i] > indices[i - 1], "must be monotonic");
        }
    }

    #[test]
    fn save_glossary_caps_at_max_entries() {
        // Mirror MAX_MEMORY_ENTRIES behaviour: oldest entries get evicted
        // when the file grows past the cap, newest survive.
        let dir = tempfile::tempdir().unwrap();
        let mut terms: Vec<TermEntry> = (0..MAX_GLOSSARY_ENTRIES + 50)
            .map(|i| TermEntry {
                term: format!("term{i}"),
                action: "translate".into(),
                target: format!("译{i}"),
                context: String::new(),
            })
            .collect();
        save_glossary(&terms, dir.path());

        // Now push a new term that should also survive.
        terms.clear();
        terms.push(TermEntry {
            term: "newest".into(),
            action: "translate".into(),
            target: "最新".into(),
            context: String::new(),
        });
        save_glossary(&terms, dir.path());

        let loaded = load_glossary(dir.path());
        assert_eq!(loaded.len(), MAX_GLOSSARY_ENTRIES);
        // The very first batch's first few entries should have been evicted.
        assert!(loaded.iter().all(|e| e.term != "term0"));
        // The newly-pushed term must be present.
        assert!(loaded.iter().any(|e| e.term == "newest"));
    }
}
