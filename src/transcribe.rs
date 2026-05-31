use crate::config::Config;
use crate::util::{self, Segment, TempFile};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub async fn run(
    input: &str,
    output: Option<&str>,
    asr: &str,
    format: Option<&str>,
    cfg: &Config,
) -> Result<PathBuf, String> {
    run_with_config_path(input, output, asr, format, cfg, None).await
}

pub async fn run_with_config_path(
    input: &str,
    output: Option<&str>,
    asr: &str,
    format: Option<&str>,
    cfg: &Config,
    config_path: Option<&Path>,
) -> Result<PathBuf, String> {
    let input_path = PathBuf::from(input);
    if !input_path.exists() {
        return Err(format!("file not found: {input}"));
    }

    let ext = format.unwrap_or("srt");
    let out_path = match output {
        Some(o) => PathBuf::from(o),
        None => input_path.with_extension(ext),
    };

    // Backends that require pre-extracted PCM WAV: bijian (uploads to a server
    // expecting raw audio) and whisper-api (HTTP multipart with audio/wav).
    //
    // Backends that decode locally — faster-whisper (PyAV) and whisper.cpp —
    // can read MP4/MKV/etc directly; pre-extracting to a 100 MB WAV first
    // wasted disk + a full ffmpeg pass. Skip the extraction for those.
    let needs_wav = matches!(asr, "bijian" | "whisper-api");
    // RAII guard for the temp WAV. `Some(...)` only when extraction
    // actually happened; `None` keeps the original input path. Drop runs on
    // every exit path (success, `?`-propagated error, panic), preventing the
    // ~100MB-per-failure orphan files we used to leak into /tmp.
    let mut _wav_guard: Option<TempFile> = None;
    let audio_path: PathBuf = if util::is_video_file(&input_path) && needs_wav {
        let wav = std::env::temp_dir().join(format!(
            "subforge_{}_{}.wav",
            tmp_token(&input_path),
            std::process::id()
        ));
        // Guard MUST be installed before the side-effecting call. If
        // extract_audio_wav writes a partial file and then errors out
        // (ffmpeg killed mid-stream, disk full halfway), we still need
        // drop() to clean it up. Setting the guard after `?` would leak
        // the partial WAV.
        _wav_guard = Some(TempFile::new(&wav));
        util::extract_audio_wav(&input_path, &wav).await?;
        wav
    } else {
        input_path.clone()
    };

    let segments = match asr {
        "bijian" => bijian_transcribe(&audio_path, cfg).await?,
        "whisper-api" => whisper_api_transcribe(&audio_path, cfg).await?,
        "faster-whisper" => {
            let mut cfg = cfg.clone();
            if let Some(config_path) = config_path {
                crate::commands::model::ensure_faster_whisper_model(&mut cfg, config_path).await?;
            }
            faster_whisper_transcribe(&audio_path, &cfg).await?
        }
        "whisper-cpp" => whisper_cpp_transcribe(&audio_path, cfg).await?,
        other => {
            return Err(format!(
                "unsupported ASR: {other}. Use: bijian/whisper-api/faster-whisper/whisper-cpp"
            ));
        }
    };

    // Optional LLM polish layer: identify and merge wrongly-split adjacent segments
    let segments = crate::polish::polish_segments(&segments, cfg).await;

    let content = util::render_srt(&segments);
    std::fs::write(&out_path, &content).map_err(|e| e.to_string())?;
    Ok(out_path)
}

/// Short stable token derived from the absolute input path; used to disambiguate
/// tmp files between concurrent invocations without leaking the full path.
fn tmp_token(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let h = Sha256::digest(canonical.to_string_lossy().as_bytes());
    hex::encode(&h[..6]) // 12 hex chars is plenty
}

// --- Bijian (Bilibili Bcut) ---

/// Per-request HTTP timeout for bijian endpoints. Roughly bracket the typical
/// upload latency for a 5-minute audio (≈ 3-5 MB) on a residential line; long
/// polls have their own deadline below.
const BIJIAN_HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Total wall-clock allowed for transcription polling. After this we error
/// out so a hung server doesn't dangle the CLI forever.
const BIJIAN_POLL_TIMEOUT: Duration = Duration::from_secs(300);

async fn bijian_transcribe(audio: &Path, cfg: &Config) -> Result<Vec<Segment>, String> {
    let client = crate::translate::http_client();
    let base = &cfg.bijian_base_url;

    // Get file size without loading into memory
    let metadata = tokio::fs::metadata(audio)
        .await
        .map_err(|e| e.to_string())?;
    let file_size = metadata.len() as usize;

    // 1. Request upload authorization. `model_id` here selects the upload
    //    endpoint family; bcut's API uses `8` for the standard upload pool.
    //    It is intentionally different from the polling `model_id` below
    //    (which selects the transcription model). Don't conflate the two.
    let auth_resp: Value = client
        .post(format!("{base}/resource/create"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "Bilibili/1.0.0 (https://www.bilibili.com)")
        .json(&serde_json::json!({"type": 2, "name": "audio.wav", "size": file_size, "ResourceFileType": "mp3", "model_id": "8"}))
        .timeout(BIJIAN_HTTP_TIMEOUT)
        .send().await.map_err(|e| format!("bijian auth failed: {e}"))?
        .json().await.map_err(|e| format!("bijian auth parse failed: {e}"))?;

    let upload_urls: Vec<String> = auth_resp["data"]["upload_urls"]
        .as_array()
        .ok_or("bijian: missing upload_urls")?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let upload_id = auth_resp["data"]["upload_id"]
        .as_str()
        .ok_or("bijian: missing upload_id")?;
    let resource_id = auth_resp["data"]["resource_id"]
        .as_str()
        .unwrap_or(upload_id);
    let in_boss_key = auth_resp["data"]["in_boss_key"].as_str().unwrap_or("");
    let per_size = auth_resp["data"]["per_size"]
        .as_u64()
        .unwrap_or(file_size as u64) as usize;

    // 2. Upload chunks — read each chunk lazily from disk so we don't hold
    //    the whole file (which can be hundreds of MB for long videos) in memory.
    //    Each PUT has a generous timeout; a stalled chunk fails its own request
    //    rather than dangling forever.
    let mut etags = Vec::new();
    for (i, url) in upload_urls.iter().enumerate() {
        let start = i * per_size;
        let end = ((i + 1) * per_size).min(file_size);
        if start >= file_size {
            break;
        }
        let chunk_bytes = read_file_chunk(audio, start as u64, end - start)
            .await
            .map_err(|e| format!("bijian read chunk {i}: {e}"))?;
        let chunk_len = chunk_bytes.len();
        let resp = client
            .put(url)
            .header("Content-Length", chunk_len.to_string())
            .body(chunk_bytes)
            .timeout(BIJIAN_HTTP_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("bijian upload part {i} failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("bijian upload part {i}: {}", resp.status()));
        }
        if let Some(etag) = resp.headers().get("Etag").and_then(|v| v.to_str().ok()) {
            etags.push(etag.to_string());
        }
    }

    // 3. Commit upload
    let commit_resp: Value = client
        .post(format!("{base}/resource/create/complete"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "Bilibili/1.0.0 (https://www.bilibili.com)")
        .json(&serde_json::json!({"InBossKey": in_boss_key, "ResourceId": resource_id, "Etags": etags.join(","), "UploadId": upload_id, "model_id": "8"}))
        .timeout(BIJIAN_HTTP_TIMEOUT)
        .send().await.map_err(|e| format!("bijian commit failed: {e}"))?
        .json().await.map_err(|e| format!("bijian commit parse failed: {e}"))?;
    let download_url = commit_resp["data"]["download_url"]
        .as_str()
        .ok_or("bijian: missing download_url in commit response")?;

    // 4. Create task. `model_id=8` again, uploading-pool ID.
    let task_resp: Value = client
        .post(format!("{base}/task"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "Bilibili/1.0.0 (https://www.bilibili.com)")
        .json(&serde_json::json!({"resource": download_url, "model_id": "8"}))
        .timeout(BIJIAN_HTTP_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("bijian create task failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("bijian create task parse failed: {e}"))?;
    let task_id = task_resp["data"]["task_id"]
        .as_str()
        .ok_or("bijian: missing task_id")?;
    crate::log_info!("  bijian task created: {task_id}");

    // 5. Poll for result. NOTE: `model_id=7` here — the polling endpoint takes
    //    the *transcription* model id, distinct from the upload pool. This is
    //    bcut's quirk, not a bug; verified empirically with current API.
    //
    //    Linear backoff 1s → 5s prevents hammering the API during long jobs.
    let deadline = tokio::time::Instant::now() + BIJIAN_POLL_TIMEOUT;
    let mut interval = Duration::from_secs(1);
    let max_interval = Duration::from_secs(5);
    loop {
        tokio::time::sleep(interval).await;
        interval = (interval + Duration::from_millis(500)).min(max_interval);

        let poll_resp: Value = client
            .get(format!("{base}/task/result"))
            .header("User-Agent", "Bilibili/1.0.0 (https://www.bilibili.com)")
            .query(&[("task_id", task_id), ("model_id", "7")])
            .timeout(BIJIAN_HTTP_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("bijian poll failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("bijian poll parse failed: {e}"))?;

        let state = poll_resp["data"]["state"].as_u64().unwrap_or(0);
        if state == 4 {
            let result_str = poll_resp["data"]["result"].as_str().unwrap_or("{}");
            let result: Value = serde_json::from_str(result_str).unwrap_or_default();
            return parse_bijian_utterances(&result);
        } else if state >= 5 {
            return Err("bijian transcription task failed".into());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("bijian polling timed out (300s)".into());
        }
    }
}

/// Read a chunk of `len` bytes starting at `offset` without loading the whole file.
async fn read_file_chunk(path: &Path, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(offset))
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
    Ok(buf)
}

fn parse_bijian_utterances(result: &Value) -> Result<Vec<Segment>, String> {
    let utterances = result["utterances"]
        .as_array()
        .ok_or("bijian result missing utterances")?;
    let mut segments = Vec::new();
    for (i, u) in utterances.iter().enumerate() {
        let text = u["transcript"]
            .as_str()
            .or(u["text"].as_str())
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            continue;
        }
        let start = u["start_time"].as_u64().unwrap_or(0);
        let end = u["end_time"].as_u64().unwrap_or(0);
        segments.push(Segment {
            index: i + 1,
            start_ms: start,
            end_ms: end,
            text: text.to_string(),
        });
    }
    if segments.is_empty() {
        return Err("bijian returned no segments".into());
    }
    Ok(segments)
}

// --- Whisper API ---

/// Cap an upload at 30 minutes. Without a timeout, a hung connection during
/// the multipart stream would dangle indefinitely (previously a real risk:
/// no timeout was set at all).
const WHISPER_API_TIMEOUT: Duration = Duration::from_secs(30 * 60);

async fn whisper_api_transcribe(audio: &Path, cfg: &Config) -> Result<Vec<Segment>, String> {
    let api_key = cfg.asr_api_key();
    let base_url = cfg.asr_base_url();
    if api_key.is_empty() || base_url.is_empty() {
        return Err("whisper-api requires api_key + base_url (or asr_api_key/asr_base_url)".into());
    }

    // Stream the file rather than loading it all at once. For a 1-hour wav (~100MB)
    // this keeps memory roughly constant instead of doubling (file + multipart copy).
    let file = tokio::fs::File::open(audio)
        .await
        .map_err(|e| e.to_string())?;
    let file_size = file.metadata().await.map_err(|e| e.to_string())?.len();
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = reqwest::Body::wrap_stream(stream);
    let part = reqwest::multipart::Part::stream_with_length(body, file_size)
        .file_name(
            audio
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .text("model", cfg.whisper_api_model.clone())
        .text("response_format", "verbose_json")
        .text("timestamp_granularities[]", "segment")
        .part("file", part);

    let url = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));
    let resp: Value = crate::translate::http_client()
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form)
        .timeout(WHISPER_API_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("whisper-api request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("whisper-api parse failed: {e}"))?;

    let segments = resp["segments"]
        .as_array()
        .ok_or("whisper-api: no segments in response")?;
    Ok(segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let start = (s["start"].as_f64().unwrap_or(0.0) * 1000.0) as u64;
            let end = (s["end"].as_f64().unwrap_or(0.0) * 1000.0) as u64;
            let text = s["text"].as_str().unwrap_or("").trim().to_string();
            Segment {
                index: i + 1,
                start_ms: start,
                end_ms: end,
                text,
            }
        })
        .filter(|s| !s.text.is_empty())
        .collect())
}

// --- Faster-Whisper with SaT segmentation ---

/// Embedded copy of the Python sidecar.
///
/// The script is `include_str!`-ed into the binary so distributing the
/// compiled `subforge` is enough; users don't need a parallel `scripts/`
/// directory. The on-disk copy at `scripts/transcribe_segment.py` is for
/// development only — editing it does NOT take effect unless you rebuild.
/// (See README §Development.)
const TRANSCRIBE_SCRIPT: &str = include_str!("../scripts/transcribe_segment.py");

/// Public accessor so `doctor` can verify the on-disk script matches what's
/// embedded in the binary. Out-of-date worktrees are a frequent confusion
/// source ("I edited the script but nothing changed").
pub fn embedded_transcribe_script() -> &'static str {
    TRANSCRIBE_SCRIPT
}

async fn faster_whisper_transcribe(audio: &Path, cfg: &Config) -> Result<Vec<Segment>, String> {
    // Per-process script path avoids races when two subforge instances run concurrently
    // (the script content is identical, but a parallel write+exec can still corrupt).
    let pid = std::process::id();
    let script_path = std::env::temp_dir().join(format!("subforge_transcribe_segment_{pid}.py"));
    // RAII: clean the script on every exit path (previously accumulated
    // forever in /tmp). Install the guard BEFORE write so a partial file
    // from a failing write is also cleaned up.
    let _script_guard = TempFile::new(&script_path);
    std::fs::write(&script_path, TRANSCRIBE_SCRIPT)
        .map_err(|e| format!("write script failed: {e}"))?;

    let lang = cfg.whisper_language.clone();
    let language: Option<&str> = if lang.is_empty() {
        None
    } else {
        Some(lang.as_str())
    };
    let device_display = if !cfg.cuda_gpu.trim().is_empty() && cfg.whisper_device != "cpu" {
        format!(
            "{} via CUDA_VISIBLE_DEVICES={}",
            cfg.whisper_device,
            cfg.cuda_gpu.trim()
        )
    } else {
        cfg.whisper_device.clone()
    };
    let args_json = serde_json::json!({
        "audio": audio.display().to_string(),
        "model": cfg.faster_whisper_model_arg(),
        "language": language,
        "segmenter": cfg.segmenter,
        "max_chars": cfg.max_chars_per_cue,
        "target_chars": cfg.target_chars_per_cue,
        "restore_punctuation": cfg.restore_punctuation,
        "device": cfg.whisper_device,
        "compute_type": cfg.whisper_compute_type,
        "device_display": device_display,
    });

    let python = cfg.python_path();
    // Capture stderr unconditionally so we can surface it in the error path.
    // We tee progress chatter (e.g. `Loading weights: 100%|...|`) to the user's
    // terminal in non-quiet mode by reading the captured bytes back later.
    let quiet = matches!(crate::logging::level(), crate::logging::Level::Quiet);
    let mut child = Command::new(&python)
        .arg(&script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true) // ensure Ctrl-C / cancellation reaps the child
        .envs(faster_whisper_env(cfg))
        .spawn()
        .map_err(|e| {
            format!(
                "python not found at {python}: {e}\n\
             Run setup or: {python} -m pip install faster-whisper wtpsplit torch \
             --index-url https://download.pytorch.org/whl/cpu"
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(args_json.to_string().as_bytes())
            .await
            .map_err(|e| format!("write stdin failed: {e}"))?;
        drop(stdin);
    }

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "python stdout pipe missing".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "python stderr pipe missing".to_string())?;

    let stdout_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
            .map_err(|e| e.to_string())
    });
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stderr.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if !quiet {
                eprint!("{}", String::from_utf8_lossy(&buf[..n]));
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        Ok::<Vec<u8>, String>(bytes)
    });

    let mut heartbeat = crate::progress::Heartbeat::new("Transcribing");
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|e| format!("python exec failed: {e}"))?,
            _ = interval.tick() => heartbeat.tick(),
        }
    };
    heartbeat.finish();

    let stdout_bytes = stdout_task.await.map_err(|e| e.to_string())??;
    let stderr_bytes = stderr_task.await.map_err(|e| e.to_string())??;
    let stderr_text = String::from_utf8_lossy(&stderr_bytes).into_owned();

    if !status.success() {
        // Surface the LAST few lines of stderr (the traceback summary) and
        // the exit code. The full stderr can be enormous when models are
        // loaded — clip to the tail which is where Python's traceback lives.
        let tail: String = stderr_text
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into());
        return Err(format!(
            "transcription script failed (exit {code})\n--- python stderr (tail) ---\n{tail}\n--- end ---"
        ));
    }

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let result: TranscribeResult = serde_json::from_str(stdout.trim()).map_err(|e| {
        let preview = stdout.chars().take(300).collect::<String>();
        format!("failed to parse transcription JSON: {e}\nOutput preview: {preview}")
    })?;

    if result.segments.is_empty() {
        return Err("no segments produced".into());
    }

    Ok(result
        .segments
        .into_iter()
        .enumerate()
        .map(|(i, s)| Segment {
            index: i + 1,
            start_ms: (s.start * 1000.0) as u64,
            end_ms: (s.end * 1000.0) as u64,
            text: s.text,
        })
        .collect())
}

fn faster_whisper_env(cfg: &Config) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    if cfg!(windows) {
        env.push(("PYTHONUTF8", "1".to_string()));
        env.push(("PYTHONIOENCODING", "utf-8".to_string()));
    }
    if cfg.whisper_device != "cpu" {
        env.extend(crate::gpu::cuda_env(cfg));
    }
    env
}

#[derive(serde::Deserialize)]
struct TranscribeResult {
    segments: Vec<TranscribedSegment>,
}

#[derive(serde::Deserialize)]
struct TranscribedSegment {
    text: String,
    start: f64,
    end: f64,
}

// --- Whisper.cpp ---

async fn whisper_cpp_transcribe(audio: &Path, cfg: &Config) -> Result<Vec<Segment>, String> {
    let pid = std::process::id();
    let output_prefix = std::env::temp_dir().join(format!("subforge_whisper_cpp_{pid}"));
    let srt_output = PathBuf::from(format!("{}.srt", output_prefix.display()));
    let _ = std::fs::remove_file(&srt_output);
    // RAII cleanup so failed runs don't leave SRT droppings behind.
    let _srt_guard = TempFile::new(&srt_output);

    let binary = cfg.sidecar_path("whisper-cli");
    let model_path = &cfg.whisper_cpp_model;
    if !PathBuf::from(model_path).exists() {
        return Err(format!(
            "whisper-cpp model not found at {model_path}.\n\
             Download a ggml model and set with: subforge config set whisper_cpp_model <path>"
        ));
    }

    let status = Command::new(&binary)
        .args(["-m", model_path])
        .arg("-f")
        .arg(audio)
        .arg("-osrt")
        .arg("-of")
        .arg(&output_prefix)
        .stdout(Stdio::null())
        .stderr(
            if matches!(crate::logging::level(), crate::logging::Level::Quiet) {
                Stdio::null()
            } else {
                Stdio::inherit()
            },
        )
        .status()
        .await
        .map_err(|e| format!("whisper-cli not found at {binary}: {e}"))?;
    if !status.success() {
        return Err("whisper-cpp failed".into());
    }
    if !srt_output.exists() {
        return Err("whisper-cpp did not produce output".into());
    }

    let content = std::fs::read_to_string(&srt_output).map_err(|e| e.to_string())?;
    Ok(util::parse_srt(&content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_bijian_extracts_basic_utterances() {
        let payload = json!({
            "utterances": [
                {"transcript": "Hello world", "start_time": 0, "end_time": 1500},
                {"transcript": "Foo bar",     "start_time": 1500, "end_time": 3000},
            ]
        });
        let segs = parse_bijian_utterances(&payload).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "Hello world");
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs[0].end_ms, 1500);
        assert_eq!(segs[1].index, 2);
    }

    #[test]
    fn parse_bijian_falls_back_to_text_field() {
        // bijian rotates between "transcript" and "text" depending on
        // endpoint; both must work.
        let payload = json!({
            "utterances": [
                {"text": "用 text 字段", "start_time": 100, "end_time": 200},
            ]
        });
        let segs = parse_bijian_utterances(&payload).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "用 text 字段");
    }

    #[test]
    fn parse_bijian_skips_empty_text() {
        // Whitespace-only utterances should not become subtitle cues —
        // bijian sometimes returns these as silence boundaries.
        let payload = json!({
            "utterances": [
                {"transcript": "   ", "start_time": 0, "end_time": 100},
                {"transcript": "real", "start_time": 100, "end_time": 200},
            ]
        });
        let segs = parse_bijian_utterances(&payload).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "real");
    }

    #[test]
    fn parse_bijian_errors_on_no_utterances_key() {
        let payload = json!({"other_key": "boom"});
        assert!(parse_bijian_utterances(&payload).is_err());
    }

    #[test]
    fn parse_bijian_errors_when_all_utterances_empty() {
        let payload = json!({
            "utterances": [
                {"transcript": "", "start_time": 0, "end_time": 100},
            ]
        });
        let err = parse_bijian_utterances(&payload).unwrap_err();
        assert!(err.contains("no segments"), "{err}");
    }

    #[test]
    fn tmp_token_is_stable_for_same_path() {
        // Stable token ⇒ same input gets the same temp WAV across runs,
        // so a previous-run leftover (rare) is reused, not collided with.
        let p = std::path::Path::new("/some/video.mp4");
        let a = tmp_token(p);
        let b = tmp_token(p);
        assert_eq!(a, b);
        assert_eq!(a.len(), 12); // 6 bytes hex-encoded
    }

    #[test]
    fn tmp_token_differs_for_different_paths() {
        let a = tmp_token(std::path::Path::new("/a/x.mp4"));
        let b = tmp_token(std::path::Path::new("/b/x.mp4"));
        assert_ne!(a, b);
    }

    #[test]
    fn embedded_transcribe_script_is_nonempty_python() {
        let s = embedded_transcribe_script();
        assert!(!s.is_empty());
        // Sanity: the script starts with python boilerplate (shebang or
        // import). If the include_str! ever points at the wrong file,
        // this catches it.
        assert!(s.contains("import") || s.starts_with("#!"));
    }
}
