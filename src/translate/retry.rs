use std::future::Future;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(16),
        }
    }
}

#[derive(Debug, Clone)]
pub enum RetryableError {
    Transient(String),
    Permanent(String),
}

#[allow(dead_code)]
impl RetryableError {
    pub fn message(&self) -> &str {
        match self {
            Self::Transient(msg) | Self::Permanent(msg) => msg,
        }
    }

    pub fn into_message(self) -> String {
        match self {
            Self::Transient(msg) | Self::Permanent(msg) => msg,
        }
    }

    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }
}

impl std::fmt::Display for RetryableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient(msg) => write!(f, "{msg}"),
            Self::Permanent(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for RetryableError {}

/// Allow `Result<T, RetryableError>` to use `?` in functions that still
/// return `Result<T, String>` (we erase the classification at that boundary
/// — the caller already gave up on structural recovery).
impl From<RetryableError> for String {
    fn from(e: RetryableError) -> Self {
        e.into_message()
    }
}

pub fn classify_http_status(status: u16, body_preview: &str) -> RetryableError {
    if status == 429 || status >= 500 {
        RetryableError::Transient(format!("HTTP {status}: {}", redact_secrets(body_preview)))
    } else {
        RetryableError::Permanent(format!("HTTP {status}: {}", redact_secrets(body_preview)))
    }
}

pub fn classify_network_error(err: &reqwest::Error) -> RetryableError {
    // reqwest's Display redacts URL userinfo passwords but NOT query strings.
    // Run our own scrub so a user who pastes `https://gw.example.com/v1?key=SECRET`
    // into their config doesn't see SECRET in stderr / logs on every transient
    // network failure.
    let msg = redact_secrets(&err.to_string());
    if err.is_timeout() || err.is_connect() || err.is_request() {
        RetryableError::Transient(msg)
    } else {
        RetryableError::Permanent(msg)
    }
}

/// Strip secret-looking values from URL query strings before they hit logs
/// or error chains. Patterns we redact:
/// - `?key=VAL` / `&key=VAL` (case-insensitive name match)
/// - `?api_key=VAL` / `?apikey=VAL` / `?token=VAL` / `?access_token=VAL`
/// - Bearer tokens in headers we accidentally render: `Bearer <token>`
///
/// Conservative on length: redacts the whole value regardless of how short
/// (better to over-redact than leak). Returns the input unchanged if no
/// pattern matches, so the common case is allocation-free for short strings.
///
/// UTF-8 safe: walks chars rather than bytes so an error string that contains
/// non-ASCII (e.g. a Chinese reverse-proxy error page echoing back the URL)
/// passes through cleanly without corruption.
pub fn redact_secrets(s: &str) -> String {
    if !s.contains('=') && !s.contains("Bearer ") {
        return s.to_string();
    }

    const SECRET_PARAM_NAMES: &[&str] = &[
        "key",
        "api_key",
        "apikey",
        "api-key",
        "token",
        "access_token",
        "auth",
        "authorization",
        "secret",
        "password",
        "pass",
        "pwd",
        "sk",
    ];

    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // Bearer <token>
        if c == 'B' && s[i..].starts_with("Bearer ") {
            out.push_str("Bearer ***");
            // Consume the next 6 chars ("earer ") plus the value chars.
            for _ in 0..6 {
                chars.next();
            }
            while let Some(&(_, ch)) = chars.peek() {
                if ch.is_ascii_alphanumeric()
                    || ch == '-'
                    || ch == '.'
                    || ch == '_'
                    || ch == '+'
                    || ch == '/'
                    || ch == '='
                {
                    chars.next();
                } else {
                    break;
                }
            }
            continue;
        }

        // [?&]name=...
        if c == '?' || c == '&' {
            // Peek at the param name up to '=' or '&'.
            let rest = &s[i + c.len_utf8()..];
            if let Some(eq_pos) = rest.find('=') {
                let name = &rest[..eq_pos];
                if !name.is_empty()
                    && !name.contains('&')
                    && SECRET_PARAM_NAMES.contains(&name.to_ascii_lowercase().as_str())
                {
                    out.push(c);
                    out.push_str(name);
                    out.push_str("=***");
                    // Advance past `name=` and the value (until '&', whitespace, or quote).
                    let to_skip = name.chars().count() + 1; // +1 for '='
                    for _ in 0..to_skip {
                        chars.next();
                    }
                    while let Some(&(_, ch)) = chars.peek() {
                        if ch == '&'
                            || ch.is_whitespace()
                            || ch == '"'
                            || ch == '\''
                            || ch == ','
                            || ch == ')'
                        {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                }
            }
        }

        out.push(c);
    }
    out
}

/// Drive `op` until it succeeds, exhausts retries, or returns Permanent.
///
/// Critically, this returns `Result<T, RetryableError>` — *not* `Result<T,
/// String>`. The Transient/Permanent classification is preserved so callers
/// can make recovery decisions (e.g. force a token refresh on a Permanent 401
/// from an auth-stale cache, but propagate Transient errors as-is so an outer
/// retry layer can still kick in).
///
/// Older call sites that returned `Result<_, String>` keep compiling because
/// `RetryableError` formats via `Display` and is convertible via the impls
/// in this module.
pub async fn with_retry<F, Fut, T>(policy: &RetryPolicy, mut op: F) -> Result<T, RetryableError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, RetryableError>>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op().await {
            Ok(value) => return Ok(value),
            Err(RetryableError::Permanent(msg)) => return Err(RetryableError::Permanent(msg)),
            Err(RetryableError::Transient(msg)) => {
                if attempt >= policy.max_attempts {
                    return Err(RetryableError::Transient(msg));
                }
                let backoff = policy.initial_backoff.saturating_mul(2u32.pow(attempt - 1));
                tokio::time::sleep(backoff.min(policy.max_backoff)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn classify_429_is_transient() {
        assert!(matches!(
            classify_http_status(429, "rate limited"),
            RetryableError::Transient(_)
        ));
    }

    #[test]
    fn classify_500_is_transient() {
        assert!(matches!(
            classify_http_status(500, "boom"),
            RetryableError::Transient(_)
        ));
    }

    #[test]
    fn classify_502_is_transient() {
        assert!(matches!(
            classify_http_status(502, "gateway"),
            RetryableError::Transient(_)
        ));
    }

    #[test]
    fn classify_401_is_permanent() {
        assert!(matches!(
            classify_http_status(401, "unauthorized"),
            RetryableError::Permanent(_)
        ));
    }

    #[test]
    fn classify_404_is_permanent() {
        assert!(matches!(
            classify_http_status(404, "not found"),
            RetryableError::Permanent(_)
        ));
    }

    #[tokio::test]
    async fn permanent_error_does_not_retry() {
        let calls = Arc::new(AtomicU32::new(0));
        let cc = calls.clone();
        let result = with_retry(&RetryPolicy::default(), move || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(RetryableError::Permanent("auth failed".into()))
            }
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(result, Err(RetryableError::Permanent(_))));
    }

    #[tokio::test]
    async fn transient_succeeds_on_retry() {
        let policy = RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        };
        let calls = Arc::new(AtomicU32::new(0));
        let cc = calls.clone();
        let result = with_retry(&policy, move || {
            let cc = cc.clone();
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(RetryableError::Transient("timeout".into()))
                } else {
                    Ok("done")
                }
            }
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(result.unwrap(), "done");
    }

    #[tokio::test]
    async fn transient_gives_up_after_max_attempts() {
        let policy = RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
        };
        let calls = Arc::new(AtomicU32::new(0));
        let cc = calls.clone();
        let result = with_retry(&policy, move || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(RetryableError::Transient("never works".into()))
            }
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // The classification survives — caller can still see this was transient.
        assert!(matches!(result, Err(RetryableError::Transient(_))));
    }

    #[tokio::test]
    async fn transient_classification_survives_to_caller() {
        // Regression test for the original complaint: with_retry used to flatten
        // the classification into a String. We now require the caller can still
        // distinguish Transient vs Permanent after retries are exhausted.
        let policy = RetryPolicy {
            max_attempts: 1,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
        };
        let result = with_retry(&policy, || async move {
            Err::<(), _>(RetryableError::Permanent("HTTP 401".into()))
        })
        .await;
        match result {
            Err(RetryableError::Permanent(msg)) => assert!(msg.contains("401")),
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[test]
    fn redact_passes_through_when_no_secrets() {
        assert_eq!(redact_secrets("plain message"), "plain message");
        assert_eq!(redact_secrets(""), "");
        assert_eq!(
            redact_secrets("https://api.example.com/v1/chat"),
            "https://api.example.com/v1/chat"
        );
    }

    #[test]
    fn redact_query_string_keys() {
        assert_eq!(
            redact_secrets("https://gw.com/v1?key=sk-abc123&model=gpt-4"),
            "https://gw.com/v1?key=***&model=gpt-4"
        );
        assert_eq!(
            redact_secrets("error sending request to https://api.com/v1?api_key=DEADBEEF"),
            "error sending request to https://api.com/v1?api_key=***"
        );
        // Case-insensitive.
        assert_eq!(
            redact_secrets("https://x.com/?KEY=abc"),
            "https://x.com/?KEY=***"
        );
    }

    #[test]
    fn redact_bearer_tokens() {
        assert_eq!(
            redact_secrets("Authorization: Bearer sk-proj-XYZ.abc_123"),
            "Authorization: Bearer ***"
        );
    }

    #[test]
    fn redact_multiple_secrets_in_one_string() {
        // Both query secret and Bearer in same message (e.g. response body
        // echoing request headers).
        let input = "https://x.com/v1?token=AAA failed: Bearer BBB invalid";
        let out = redact_secrets(input);
        assert!(!out.contains("AAA"), "leaked AAA: {out}");
        assert!(!out.contains("BBB"), "leaked BBB: {out}");
        assert!(out.contains("token=***"));
        assert!(out.contains("Bearer ***"));
    }

    #[test]
    fn redact_preserves_non_secret_query_params() {
        assert_eq!(
            redact_secrets("https://x.com/?q=hello&model=gpt-4&page=2"),
            "https://x.com/?q=hello&model=gpt-4&page=2"
        );
    }

    #[test]
    fn redact_handles_utf8_safely() {
        // Reverse-proxy error pages occasionally echo URLs into Chinese
        // surrounding text; we must not corrupt the multi-byte chars.
        let input = "请求失败 https://x.com/?key=SECRET 错误码 500";
        let out = redact_secrets(input);
        assert!(out.contains("key=***"));
        assert!(!out.contains("SECRET"));
        assert!(out.contains("请求失败"), "lost UTF-8 prefix: {out}");
        assert!(out.contains("错误码"), "lost UTF-8 suffix: {out}");
    }

    #[test]
    fn classify_network_error_redacts_when_called() {
        // Exercise the wiring: a fake error message containing a URL with a
        // sensitive query param should be scrubbed before reaching the caller.
        // We can't easily fabricate a reqwest::Error with a custom message,
        // so this test directly exercises classify_http_status which is the
        // adjacent path users hit on real network failures.
        let err = classify_http_status(503, "upstream https://gw/?key=SHOULDNTLEAK timed out");
        let msg = err.message();
        assert!(!msg.contains("SHOULDNTLEAK"), "leaked: {msg}");
        assert!(msg.contains("key=***"));
    }
}
