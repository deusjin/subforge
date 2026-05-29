pub mod batch;
pub mod bing;
pub mod google;
pub mod llm;
pub mod pipeline;
pub mod quality;
pub mod retry;
pub mod tm;

use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::config::Config;
use crate::progress::ProgressBar;
use crate::util::Segment;

use self::retry::with_retry;

/// Shared HTTP client — reuses connection pool and TLS sessions across all
/// requests in a process lifetime. Creating a new `reqwest::Client` per
/// request wastes ~100-200ms on TLS negotiation each time.
///
/// Configuration:
/// - `connect_timeout(10s)`: a misconfigured `base_url` pointing to an
///   unreachable host otherwise hangs ~2 minutes per attempt (kernel TCP
///   retry). With retry on top, that's >5 minutes per failed request.
/// - `pool_idle_timeout(60s)`: drop sockets to upstreams that may rotate
///   IPs (common with LLM gateways behind CloudFlare).
/// - No global `timeout(...)`: per-request `timeout()` calls (15-1800s
///   depending on operation) carry the right value. A blanket cap here
///   would break the long-running whisper-api upload (30 min) for instance.
/// - TLS: defaults verify certs. We never set `danger_accept_invalid_certs`.
pub fn http_client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    use std::time::Duration;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(60))
            .user_agent(concat!("subforge/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build should not fail with default features")
    })
}

/// Top-level entry point. Dispatches to the LLM pipeline or simple web translators.
pub async fn translate_batch(
    texts: &[String],
    segments: &[Segment],
    cfg: &Config,
    context_dir: Option<&std::path::Path>,
) -> Result<Vec<String>, String> {
    if cfg.translator.is_empty() || texts.is_empty() {
        return Ok(vec![String::new(); texts.len()]);
    }

    match cfg.translator.as_str() {
        "llm" => translate_llm(texts, segments, cfg, context_dir).await,
        "bing" | "google" => translate_web_concurrent(texts, cfg).await,
        other => Err(format!("unsupported translator: {other}")),
    }
}

/// LLM pipeline: a thin orchestrator that runs each stage in order.
/// All real work lives in the `pipeline` module's stage functions.
async fn translate_llm(
    texts: &[String],
    segments: &[Segment],
    cfg: &Config,
    context_dir: Option<&std::path::Path>,
) -> Result<Vec<String>, String> {
    let mut state = pipeline::PipelineState::new(texts, segments, context_dir, cfg);

    pipeline::stage_extract_terminology(&mut state, cfg).await;
    pipeline::stage_load_memory(&mut state);
    pipeline::stage_translate(&mut state, cfg).await?;
    pipeline::stage_quality_estimation(&mut state, cfg).await;
    pipeline::stage_save_tm(&state, cfg);

    Ok(state.results)
}

/// Simple concurrent web-translator path (Google / Bing). No LLM, no quality pipeline.
///
/// Google gets wrapped in `with_retry` since it has no internal retry logic.
/// Bing already has internal retry + 401 token refresh, so we call it directly
/// to avoid double-retry (which would cause 63s waits on genuine failures).
async fn translate_web_concurrent(texts: &[String], cfg: &Config) -> Result<Vec<String>, String> {
    let semaphore = Arc::new(Semaphore::new(cfg.safe_thread_num()));
    let mut tasks = JoinSet::new();

    for (idx, text) in texts.iter().enumerate() {
        let sem = semaphore.clone();
        let translator = cfg.translator.clone();
        let target = cfg.target_language.clone();
        let text = text.clone();
        tasks.spawn(async move {
            let _permit = sem.acquire().await.map_err(|e| e.to_string())?;
            let translated = match translator.as_str() {
                "bing" => bing::translate(&text, &target).await,
                "google" => {
                    let policy = retry::RetryPolicy::default();
                    with_retry(&policy, || {
                        let target = target.clone();
                        let text = text.clone();
                        async move {
                            google::translate(&text, &target)
                                .await
                                .map_err(retry::RetryableError::Transient)
                        }
                    })
                    .await
                    .map_err(|e| e.into_message())
                }
                _ => unreachable!(),
            }?;
            Ok::<(usize, String), String>((idx, translated))
        });
    }

    let mut results = vec![String::new(); texts.len()];
    let mut progress = ProgressBar::new("Translating", texts.len());
    while let Some(joined) = tasks.join_next().await {
        let (idx, translated) = joined.map_err(|e| e.to_string())??;
        results[idx] = translated;
        progress.inc(1);
    }
    progress.finish();
    Ok(results)
}
