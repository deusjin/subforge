//! Batched LLM translation with two execution strategies.
//!
//! # The fundamental tension
//!
//! "Moving window" means batch K's prompt includes batch K-1's translations as
//! context. This is **inherently sequential**: K must wait for K-1's result.
//! Concurrency between batches breaks the window.
//!
//! # Two strategies (selectable via `cfg.chained_translation`)
//!
//! ## Chained (default, `chained_translation = true`)
//!
//! All batches run sequentially. Each batch sees the running results vector,
//! including all previously-translated cues. Window is genuinely moving.
//!
//! - **Pro**: Best translation continuity. Refers to fresh, in-document context.
//! - **Con**: Total time ≈ sum(per-batch latency). For 80 batches @ 5s each = ~400s.
//!
//! ## Concurrent (`chained_translation = false`)
//!
//! First `SEQ_BATCHES` (=3) batches run sequentially as a warm-up. The
//! remainder run as **rolling waves** of `thread_num` parallel batches: each
//! wave waits for the previous wave to finish, then computes its window
//! against everything translated so far. Within a wave, batches share the
//! same window (we sacrifice in-wave continuity for parallelism).
//!
//! - **Pro**: ~thread_num × faster on long videos.
//! - **Con**: Adjacent in-wave batches don't see each other's translations.
//!   Cross-wave continuity is preserved.
//!
//! # When to use which
//!
//! - Short videos (< 5 min): chained is fine, time difference negligible.
//! - Critical content (subtitles for distribution): chained.
//! - Long videos where speed matters more than perfect continuity: concurrent.
//! - When you have rate-limited APIs: chained also avoids burst limits.

use super::{
    llm::{self, SubtitleCue},
    tm,
};
use crate::config::Config;
use crate::progress::ProgressBar;
use crate::util::Segment;

/// Number of warm-up batches (sequential) when running in concurrent mode.
const SEQ_BATCHES: usize = 3;
/// Size of the rolling translation context shown to each batch.
const WINDOW_SIZE: usize = 5;

pub async fn two_phase_translate(
    texts: &[String],
    segments: &[Segment],
    cfg: &Config,
    terms_prompt: &str,
    memory: &[tm::MemoryEntry],
) -> Result<Vec<String>, String> {
    let cues = build_cues(texts, segments);
    let chunks: Vec<&[SubtitleCue]> = cues.chunks(cfg.safe_batch_size()).collect();

    if cfg.chained_translation {
        chained_translate(&cues, &chunks, cfg, terms_prompt, memory).await
    } else {
        concurrent_translate(&cues, &chunks, cfg, terms_prompt, memory).await
    }
}

/// Sequentially translate every batch. Each batch's prompt sees prior batches'
/// translations as a true moving window.
async fn chained_translate(
    cues: &[SubtitleCue],
    chunks: &[&[SubtitleCue]],
    cfg: &Config,
    terms_prompt: &str,
    memory: &[tm::MemoryEntry],
) -> Result<Vec<String>, String> {
    let total = chunks.len();
    let mut results: Vec<String> = vec![String::new(); cues.len()];
    let mut progress = ProgressBar::new("LLM translating", total);

    for (chunk_idx, chunk) in chunks.iter().enumerate() {
        // Build context AFTER previous batch wrote to results — true moving window.
        let extra = build_batch_context(terms_prompt, memory, chunk, &results, cues);
        let translations = llm::translate_batch(
            chunk,
            &cfg.target_language,
            &extra,
            &cfg.api_key,
            &cfg.base_url,
            &cfg.model,
        )
        .await?;
        for (i, t) in translations.into_iter().enumerate() {
            let global = chunk_idx * cfg.safe_batch_size() + i;
            if global < results.len() {
                results[global] = t;
            }
        }
        progress.inc(1);
        crate::log_debug!("batch {}/{total} done (chained)", chunk_idx + 1);
    }
    progress.finish();

    Ok(results)
}

/// Warm-up sequentially, then run remaining batches in concurrent **waves**
/// of size `thread_num`. Each wave waits for the previous wave to finish
/// before computing its context window — so the window genuinely rolls
/// forward as work progresses, instead of being frozen at the end of warm-up.
///
/// This is the fix for the issue described in the audit: the prior
/// implementation built one frozen context from warm-up state and reused it
/// for ALL phase-2 batches. The 100th batch saw the same 21-cue window the
/// 4th batch saw — effectively "no window" past warm-up. Wave-based
/// scheduling preserves most of the throughput win while restoring continuity.
async fn concurrent_translate(
    cues: &[SubtitleCue],
    chunks: &[&[SubtitleCue]],
    cfg: &Config,
    terms_prompt: &str,
    memory: &[tm::MemoryEntry],
) -> Result<Vec<String>, String> {
    let total = chunks.len();
    let seq_count = SEQ_BATCHES.min(total);
    let batch_size = cfg.safe_batch_size();
    let wave_size = cfg.safe_thread_num();
    let mut results: Vec<String> = vec![String::new(); cues.len()];
    let mut progress = ProgressBar::new("LLM translating", total);

    // Phase 1: warm-up. Same as chained, just bounded.
    for (chunk_idx, chunk) in chunks.iter().take(seq_count).enumerate() {
        let extra = build_batch_context(terms_prompt, memory, chunk, &results, cues);
        let translations = llm::translate_batch(
            chunk,
            &cfg.target_language,
            &extra,
            &cfg.api_key,
            &cfg.base_url,
            &cfg.model,
        )
        .await?;
        for (i, t) in translations.into_iter().enumerate() {
            let idx = chunk_idx * batch_size + i;
            if idx < results.len() {
                results[idx] = t;
            }
        }
        progress.inc(1);
        crate::log_debug!("batch {}/{total} done (warmup)", chunk_idx + 1);
    }

    if seq_count >= total {
        progress.finish();
        return Ok(results);
    }

    // Phase 2: rolling waves of `wave_size` concurrent batches.
    let mut completed = seq_count;
    let mut chunk_idx = seq_count;
    while chunk_idx < total {
        let wave_end = (chunk_idx + wave_size).min(total);
        let mut handles = Vec::new();

        for (inner_idx, chunk_ref) in chunks.iter().enumerate().take(wave_end).skip(chunk_idx) {
            let chunk = chunk_ref.to_vec();
            // Build context FRESH at the start of every wave so the rolling
            // window incorporates everything written by all previous waves
            // (and the warm-up). Within a single wave the context is shared
            // across the parallel batches — that's the design trade-off:
            // continuity between adjacent in-wave batches is sacrificed for
            // throughput, but cross-wave continuity is preserved.
            let extra = build_batch_context(terms_prompt, memory, &chunk, &results, cues);
            let target_lang = cfg.target_language.clone();
            let api_key = cfg.api_key.clone();
            let base_url = cfg.base_url.clone();
            let model = cfg.model.clone();
            let offset = inner_idx * batch_size;

            handles.push(tokio::spawn(async move {
                let translations =
                    llm::translate_batch(&chunk, &target_lang, &extra, &api_key, &base_url, &model)
                        .await?;
                Ok::<(usize, Vec<String>), String>((offset, translations))
            }));
        }

        // Wait for the whole wave before starting the next so the rolling
        // window has fresh data to pull from.
        for handle in handles {
            match handle.await {
                Ok(Ok((offset, translations))) => {
                    for (i, t) in translations.into_iter().enumerate() {
                        if offset + i < results.len() {
                            results[offset + i] = t;
                        }
                    }
                    completed += 1;
                    progress.inc(1);
                    crate::log_debug!("batch {completed}/{total} done (wave)");
                }
                Ok(Err(e)) => return Err(e),
                Err(e) => return Err(e.to_string()),
            }
        }

        chunk_idx = wave_end;
    }
    progress.finish();

    Ok(results)
}

fn build_cues(texts: &[String], segments: &[Segment]) -> Vec<SubtitleCue> {
    texts
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let duration_ms = if i < segments.len() {
                segments[i]
                    .end_ms
                    .saturating_sub(segments[i].start_ms)
                    .max(100)
            } else {
                2000
            };
            SubtitleCue {
                index: i + 1,
                text: text.clone(),
                duration_ms,
            }
        })
        .collect()
}

/// Build per-batch system prompt extras: terminology + TM retrieval + window.
fn build_batch_context(
    terms_prompt: &str,
    memory: &[tm::MemoryEntry],
    chunk: &[SubtitleCue],
    results: &[String],
    all_cues: &[SubtitleCue],
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if !terms_prompt.is_empty() {
        parts.push(terms_prompt.to_string());
    }

    if !memory.is_empty() {
        let batch_texts: Vec<&str> = chunk.iter().map(|c| c.text.as_str()).collect();
        let tm_prompt = tm::retrieve_for_batch(memory, &batch_texts, 2);
        if !tm_prompt.is_empty() {
            parts.push(tm_prompt);
        }
    }

    if let Some(first) = chunk.first() {
        let first_idx = all_cues
            .iter()
            .position(|c| c.index == first.index)
            .unwrap_or(0);
        let mut window: Vec<(String, String)> = Vec::new();
        for i in (0..first_idx).rev() {
            if window.len() >= WINDOW_SIZE {
                break;
            }
            if !results[i].is_empty() {
                window.push((all_cues[i].text.clone(), results[i].clone()));
            }
        }
        window.reverse();
        let prompt = tm::format_moving_window(&window);
        if !prompt.is_empty() {
            parts.push(prompt);
        }
    }

    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::llm::SubtitleCue;

    fn cue(i: usize, text: &str) -> SubtitleCue {
        SubtitleCue {
            index: i,
            text: text.into(),
            duration_ms: 2000,
        }
    }

    #[test]
    fn build_batch_context_includes_window_when_results_present() {
        let cues = vec![cue(1, "hello"), cue(2, "world"), cue(3, "foo")];
        let results = vec!["你好".to_string(), "世界".to_string(), String::new()];
        let chunk = &cues[2..]; // chunk starts at cue index 3
        let ctx = build_batch_context("", &[], chunk, &results, &cues);
        // Should reference prior translations
        assert!(ctx.contains("你好"));
        assert!(ctx.contains("世界"));
    }

    #[test]
    fn build_batch_context_skips_empty_results() {
        // If all prior results are empty (e.g. concurrent mode hasn't filled them),
        // window should be empty rather than referencing empty translations.
        let cues = vec![cue(1, "hello"), cue(2, "world"), cue(3, "foo")];
        let results = vec![String::new(); 3];
        let chunk = &cues[2..];
        let ctx = build_batch_context("", &[], chunk, &results, &cues);
        assert!(!ctx.contains("Recent translations"));
    }

    #[test]
    fn build_batch_context_first_chunk_has_no_window() {
        let cues = vec![cue(1, "hello"), cue(2, "world")];
        let results = vec![String::new(); 2];
        let chunk = &cues[..2]; // first chunk
        let ctx = build_batch_context("", &[], chunk, &results, &cues);
        assert!(!ctx.contains("Recent translations"));
    }
}
