//! Bing Edge translator with refreshable auth token.
//!
//! Bing's `edge.microsoft.com/translate/auth` endpoint hands out a JWT that
//! expires in roughly 10 minutes. The original implementation cached it in a
//! `OnceCell` and never refreshed — so a long video that translated for more
//! than 10 minutes would start failing with 401 mid-batch and the failures
//! were unrecoverable (the web translator path had no retry).
//!
//! This module fixes that with three layers of defense:
//!
//! 1. **Time-based refresh.** The token store records `fetched_at`; we treat
//!    any token older than [`TOKEN_TTL`] (8 min, comfortably under the ~10
//!    min Bing limit) as expired and re-fetch before making the request.
//!
//! 2. **401 reactive refresh.** If a translate call comes back with HTTP 401
//!    despite a "fresh" token (clock skew, server-side eviction, weird edge
//!    case), we force-refresh once and retry the same call. The retry budget
//!    is one — we don't loop on a genuinely bad credential.
//!
//! 3. **Transient retry.** Network glitches (timeout, connection reset) flow
//!    through `with_retry`'s Transient classification with exponential
//!    backoff, same as the LLM path.

use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::retry::{
    RetryPolicy, RetryableError, classify_http_status, classify_network_error, with_retry,
};

/// How long to trust a cached token before proactively refreshing.
/// Bing's actual server-side lifetime is ~10 min; staying well below avoids
/// races where a token expires mid-flight.
const TOKEN_TTL: Duration = Duration::from_secs(8 * 60);

#[derive(Clone)]
struct TokenEntry {
    value: String,
    fetched_at: Instant,
}

impl TokenEntry {
    fn is_fresh(&self) -> bool {
        self.fetched_at.elapsed() < TOKEN_TTL
    }
}

static BING_TOKEN: Mutex<Option<TokenEntry>> = Mutex::const_new(None);

/// Return a usable token, refreshing if expired.
async fn get_token() -> Result<String, String> {
    let mut guard = BING_TOKEN.lock().await;
    if let Some(t) = guard.as_ref()
        && t.is_fresh()
    {
        return Ok(t.value.clone());
    }
    let token = fetch_token().await?;
    *guard = Some(TokenEntry {
        value: token.clone(),
        fetched_at: Instant::now(),
    });
    Ok(token)
}

/// Force a refresh regardless of cached state. Called after a 401 mid-call.
async fn force_refresh_token() -> Result<String, String> {
    let mut guard = BING_TOKEN.lock().await;
    let token = fetch_token().await?;
    *guard = Some(TokenEntry {
        value: token.clone(),
        fetched_at: Instant::now(),
    });
    Ok(token)
}

/// Hit Bing Edge's auth endpoint via curl. We use curl rather than reqwest
/// because Bing fingerprints TLS clients and rejects anything that doesn't
/// look like a real browser/curl handshake.
async fn fetch_token() -> Result<String, String> {
    let output = tokio::process::Command::new("curl")
        .args(["-s", "https://edge.microsoft.com/translate/auth"])
        .output()
        .await
        .map_err(|e| format!("bing auth (curl) failed: {e}"))?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.len() < 100 {
        return Err(format!(
            "bing auth returned invalid token (len={})",
            token.len()
        ));
    }
    Ok(token)
}

/// Translate one text through Bing. Retries on transient failures (timeouts,
/// 429, 5xx) and force-refreshes the token once on a 401.
pub async fn translate(text: &str, target_lang: &str) -> Result<String, String> {
    let lang_code = resolve_bing_lang(target_lang);
    let policy = RetryPolicy::default();

    // Outer retry handles transient network/HTTP failures with exponential
    // backoff. Auth failures are surfaced as Permanent so we react below.
    let result = with_retry(&policy, || {
        let lang_code = lang_code.clone();
        let text = text.to_string();
        async move {
            translate_once(&text, &lang_code, /*force_refresh=*/ false).await
        }
    })
    .await;

    match result {
        Ok(s) => Ok(s),
        Err(RetryableError::Permanent(msg)) if msg.contains("HTTP 401") => {
            // Token cache thinks it's fresh but the server disagrees. Force
            // a refresh and try one more time. If this also 401s, it's a
            // genuine auth failure, not staleness.
            translate_once(text, &lang_code, /*force_refresh=*/ true)
                .await
                .map_err(|e| e.into_message())
        }
        Err(e) => Err(e.into_message()),
    }
}

/// Single translate attempt. `force_refresh=true` invalidates the cached
/// token before sending the request — used as the recovery path after 401.
async fn translate_once(
    text: &str,
    lang_code: &str,
    force_refresh: bool,
) -> Result<String, RetryableError> {
    let token = if force_refresh {
        force_refresh_token().await
    } else {
        get_token().await
    }
    .map_err(|e| {
        // Auth-server failures are network-ish, not user errors; retry.
        RetryableError::Transient(e)
    })?;

    let response = super::http_client()
        .post("https://api-edge.cognitive.microsofttranslator.com/translate")
        .query(&[("to", lang_code), ("api-version", "3.0")])
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!([{"Text": text}]))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| classify_network_error(&e))?;

    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| RetryableError::Transient(format!("bing read body failed: {e}")))?;

    if !(200..300).contains(&status) {
        let preview: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        return Err(classify_http_status(status, &preview));
    }

    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| RetryableError::Transient(format!("bing response parse failed: {e}")))?;

    json[0]["translations"][0]["text"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| {
            let preview: String = json.to_string().chars().take(200).collect();
            RetryableError::Permanent(format!("bing unexpected response: {preview}"))
        })
}

fn resolve_bing_lang(lang: &str) -> String {
    match lang.to_ascii_lowercase().as_str() {
        "zh-hans" | "zh-cn" | "chinese" => "zh-Hans",
        "zh-hant" | "zh-tw" => "zh-Hant",
        "en" | "english" => "en",
        "ja" | "japanese" => "ja",
        "ko" | "korean" => "ko",
        "fr" | "french" => "fr",
        "de" | "german" => "de",
        "es" | "spanish" => "es",
        _ => lang,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_freshness_respects_ttl() {
        let t = TokenEntry {
            value: "x".into(),
            fetched_at: Instant::now(),
        };
        assert!(t.is_fresh());
        // We can't easily fast-forward Instant in a unit test without a clock
        // shim, but constructing one in the past simulates expiry.
        let stale = TokenEntry {
            value: "x".into(),
            fetched_at: Instant::now() - TOKEN_TTL - Duration::from_secs(1),
        };
        assert!(!stale.is_fresh());
    }

    #[test]
    fn lang_resolution() {
        assert_eq!(resolve_bing_lang("zh-Hans"), "zh-Hans");
        assert_eq!(resolve_bing_lang("zh-cn"), "zh-Hans");
        assert_eq!(resolve_bing_lang("japanese"), "ja");
        assert_eq!(resolve_bing_lang("xx-custom"), "xx-custom");
    }
}
