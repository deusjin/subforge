use serde_json::{Value, json};
use std::time::Duration;

use super::retry::{
    RetryPolicy, RetryableError, classify_http_status, classify_network_error, with_retry,
};

/// A single subtitle cue for batch translation
#[derive(Debug, Clone)]
pub struct SubtitleCue {
    pub index: usize,
    pub text: String,
    pub duration_ms: u64,
}

/// Translate a batch of subtitle cues with retry and per-cue fallback.
/// Returns translations in the same order as input cues.
pub async fn translate_batch(
    cues: &[SubtitleCue],
    target_lang: &str,
    system_extra: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Result<Vec<String>, String> {
    if cues.is_empty() {
        return Ok(vec![]);
    }
    if api_key.is_empty() || base_url.is_empty() {
        return Err("LLM translation requires api_key and base_url".into());
    }

    let system = build_system_prompt(cues, target_lang, system_extra);
    let user = build_user_prompt(cues);
    let expected = cues.len();

    // Try batch with retry
    let policy = RetryPolicy::default();
    let batch_result = with_retry(&policy, || {
        let system = system.clone();
        let user = user.clone();
        async move {
            let raw = call_llm(api_key, base_url, model, &system, &user).await?;
            parse_batch_response(&raw, expected).map_err(RetryableError::Transient)
        }
    })
    .await;

    match batch_result {
        Ok(translations) => Ok(translations),
        Err(batch_err) => {
            let preview_str = preview(batch_err.message(), 80);
            crate::log_debug!("batch failed ({preview_str}), falling back to per-cue...");
            translate_per_cue(cues, target_lang, system_extra, api_key, base_url, model).await
        }
    }
}

/// Fallback: translate each cue individually
async fn translate_per_cue(
    cues: &[SubtitleCue],
    target_lang: &str,
    system_extra: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Result<Vec<String>, String> {
    let policy = RetryPolicy {
        max_attempts: 2,
        ..Default::default()
    };
    let mut results = Vec::with_capacity(cues.len());

    for cue in cues {
        let max_chars = compute_max_chars(cue.duration_ms, target_lang);
        let system = format!(
            "Translate the subtitle to {target_lang}. Max {max_chars} characters. \
             Return ONLY the translation.\n{system_extra}"
        );
        let result = with_retry(&policy, || {
            let system = system.clone();
            let text = cue.text.clone();
            async move {
                let raw = call_llm(api_key, base_url, model, &system, &text).await?;
                Ok(raw.trim().trim_matches('"').to_string())
            }
        })
        .await;

        results.push(result.unwrap_or_else(|_| cue.text.clone()));
    }
    Ok(results)
}

/// Single text translation (for non-batch use)
#[allow(dead_code)]
pub async fn translate_single(
    text: &str,
    target_lang: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Result<String, String> {
    if api_key.is_empty() || base_url.is_empty() {
        return Err("LLM translation requires api_key and base_url".into());
    }
    let system = format!("Translate to {target_lang}. Return ONLY the translation.");
    let policy = RetryPolicy::default();
    let result = with_retry(&policy, || {
        let system = system.clone();
        async move {
            let raw = call_llm(api_key, base_url, model, &system, text).await?;
            Ok(raw.trim().trim_matches('"').to_string())
        }
    })
    .await
    .map_err(String::from)?;
    Ok(result)
}

/// Generic LLM chat call used by quality module too
pub async fn call_chat_with_system(
    api_key: &str,
    base_url: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let policy = RetryPolicy::default();
    with_retry(&policy, || async move {
        call_llm(api_key, base_url, model, system, user).await
    })
    .await
    .map_err(String::from)
}

// --- Internal ---

fn build_system_prompt(cues: &[SubtitleCue], target_lang: &str, extra: &str) -> String {
    let n = cues.len();
    let mut prompt = format!(
        "You are translating video subtitles into {target_lang}.\n\n\
         Rules:\n\
         1. Return a JSON array of {n} translated strings, in the SAME ORDER.\n\
         2. Array length MUST equal {n}.\n\
         3. Each cue has a max_chars limit based on screen duration. Be concise.\n\
         4. Keep terminology consistent across cues.\n\
         5. No explanations, speaker labels, or quotation marks.\n\
         6. Preserve line breaks within a cue (use \\n in JSON strings)."
    );
    if !extra.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    prompt
}

fn build_user_prompt(cues: &[SubtitleCue]) -> String {
    let items: Vec<Value> = cues
        .iter()
        .map(|cue| {
            let max_chars = compute_max_chars(cue.duration_ms, "");
            json!({
                "i": cue.index,
                "duration": format!("{:.1}s", cue.duration_ms as f64 / 1000.0),
                "max_chars": max_chars,
                "text": cue.text,
            })
        })
        .collect();
    let json_str = serde_json::to_string_pretty(&items).unwrap_or_default();
    format!(
        "{json_str}\n\nReturn ONLY a JSON array of {} strings.",
        cues.len()
    )
}

fn compute_max_chars(duration_ms: u64, target_lang: &str) -> usize {
    let duration_secs = duration_ms as f64 / 1000.0;
    let cps = max_cps(target_lang);
    (duration_secs * cps).floor().max(4.0) as usize
}

fn max_cps(target_lang: &str) -> f64 {
    let lang = target_lang.to_ascii_lowercase();
    if lang.starts_with("zh") || lang.contains("chinese") {
        9.0
    } else if lang.starts_with("ja") || lang.contains("japanese") {
        4.0
    } else if lang.starts_with("ko") || lang.contains("korean") {
        7.0
    } else {
        17.0
    }
}

async fn call_llm(
    api_key: &str,
    base_url: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, RetryableError> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0,
        "stream": false
    });

    let response = super::http_client()
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| classify_network_error(&e))?;

    let status = response.status().as_u16();

    // Read raw bytes first so we can preserve the HTTP status even when the body
    // is HTML (typical for 502/504 from gateways/proxies).
    let bytes = response
        .bytes()
        .await
        .map_err(|e| RetryableError::Transient(format!("read body failed: {e}")))?;

    if !(200..300).contains(&status) {
        // Non-2xx: classify by status, include short body preview for diagnostics.
        let preview: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        return Err(classify_http_status(status, &preview));
    }

    // 2xx: now parse JSON
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|e| RetryableError::Transient(format!("response parse failed: {e}")))?;

    extract_content(&body)
        .ok_or_else(|| RetryableError::Permanent(format!("unexpected response format: {body}")))
}

fn extract_content(payload: &Value) -> Option<String> {
    payload
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|s| s.trim().to_string())
}

fn parse_batch_response(raw: &str, expected: usize) -> Result<Vec<String>, String> {
    // Try direct parse
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(raw)
        && arr.len() == expected
    {
        return Ok(arr);
    }
    // Try extracting from ```json ... ```
    if let Some(s) = extract_code_block(raw)
        && let Ok(arr) = serde_json::from_str::<Vec<String>>(&s)
        && arr.len() == expected
    {
        return Ok(arr);
    }
    // Try finding first JSON array
    if let Some(s) = extract_first_json_array(raw)
        && let Ok(arr) = serde_json::from_str::<Vec<String>>(&s)
        && arr.len() == expected
    {
        return Ok(arr);
    }
    Err(format!(
        "batch count mismatch or parse failed (expected {expected}): {}",
        preview(raw, 150)
    ))
}

fn extract_code_block(text: &str) -> Option<String> {
    for marker in ["```json\n", "```\n"] {
        if let Some(start) = text.find(marker) {
            let content_start = start + marker.len();
            if let Some(end) = text[content_start..].find("```") {
                return Some(text[content_start..content_start + end].trim().to_string());
            }
        }
    }
    None
}

fn extract_first_json_array(text: &str) -> Option<String> {
    let start = text.find('[')?;
    let bytes = &text.as_bytes()[start..];
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape = true,
            b'"' => in_string = !in_string,
            b'[' if !in_string => depth += 1,
            b']' if !in_string => {
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

fn preview(s: &str, max: usize) -> String {
    let truncated: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{truncated}...")
    } else {
        truncated
    }
}
