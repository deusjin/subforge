use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::log_info;
use crate::progress::format_elapsed;
use crate::{cache, config::Config, subtitle, synthesize, transcribe, util};

/// Convert a `Path` to a `&str` or surface a clear error.
///
/// On Unix non-UTF-8 bytes are technically valid in paths; on Windows the
/// native form is UTF-16 and round-tripping through `&str` may fail for
/// rarely-encoded characters. Either way we'd rather fail with a friendly
/// message than panic.
fn path_str<'a>(p: &'a Path, label: &str) -> Result<&'a str, String> {
    p.to_str().ok_or_else(|| {
        format!(
            "{label} 路径不是有效的 UTF-8: {} (subforge 内部接口当前要求 UTF-8 路径)",
            p.display()
        )
    })
}

/// Pipeline knobs that aren't related to *what* to translate, just *how* to
/// run the pipeline. Grouped to keep `run`'s signature stable as we add flags.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Skip the burn-in step. Used by `subforge translate`.
    pub no_synthesize: bool,
    /// Bypass the file cache for transcribe/translate.
    pub no_cache: bool,
    /// When true, keep the intermediate `<stem>.srt` and `<stem>_translated.srt`
    /// next to the final output. Default is false: only the final artifact
    /// (translated SRT or burned video) survives.
    pub keep_intermediate: bool,
    /// Synthesis options forwarded to `synthesize::run`. Lets the user pick
    /// soft / hard / both mode through the all-in-one `process` command.
    pub synth: synthesize::Options,
}

/// Resolve the user's `--output` argument into a (working_dir, optional_final_path)
/// pair, working around the longstanding ambiguity of "is this a directory or
/// a file path?".
///
/// Rules (in order):
/// - No `-o`: emit alongside the input video. final_path is None (default
///   `<stem>_translated.srt` or `<stem>_captioned.<ext>` will be derived).
/// - `-o foo/` (trailing separator) or existing directory: treat as a
///   directory; final_path is None.
/// - `-o foo.srt` / `foo.mp4` (looks like a file: has a non-empty extension
///   and the path doesn't already exist as a directory): treat as the
///   FINAL output filename. We use its parent as the working dir.
/// - Anything else: treat as a directory. We never silently `mkdir` a
///   user-typed file name as a folder (that was the old bug).
fn resolve_output(input: &Path, output: Option<&str>) -> (PathBuf, Option<PathBuf>) {
    let Some(o) = output else {
        return (input.parent().unwrap_or(Path::new(".")).to_path_buf(), None);
    };
    let path = PathBuf::from(o);
    let trailing_sep = o.ends_with('/') || o.ends_with(std::path::MAIN_SEPARATOR);
    let exists_as_dir = path.is_dir();
    let looks_like_file =
        !trailing_sep && !exists_as_dir && path.extension().is_some_and(|e| !e.is_empty());

    if looks_like_file {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        (dir, Some(path))
    } else {
        (path, None)
    }
}

/// Run the full pipeline: transcribe → translate → (optionally) synthesize.
///
/// The TM directory is anchored to the **original input video's** parent.
/// This is the only entry point that knows where the video originally was;
/// `subtitle::run` called standalone uses the SRT's parent. We thread the
/// override through `subtitle::run(..., Some(tm_anchor))` so both invocation
/// paths produce identical TM placement for the same video.
pub async fn run(
    input: &str,
    output: Option<&str>,
    opts: Options,
    cfg: &Config,
) -> Result<PathBuf, String> {
    run_with_config_path(input, output, opts, cfg, None).await
}

pub async fn run_with_config_path(
    input: &str,
    output: Option<&str>,
    opts: Options,
    cfg: &Config,
    config_path: Option<&Path>,
) -> Result<PathBuf, String> {
    let input_path = PathBuf::from(input);
    if !input_path.exists() {
        return Err(format!("input file not found: {input}"));
    }
    let total_started = Instant::now();

    let stem = input_path.file_stem().unwrap_or_default().to_string_lossy();
    let (out_dir, explicit_final) = resolve_output(&input_path, output);
    std::fs::create_dir_all(&out_dir).ok();

    // TM anchors to the original video, not the output dir. Same anchor is
    // used regardless of whether the user calls `process` or just `subtitle`
    // on a previously-transcribed SRT — addresses the inconsistency where
    // identical inputs ended up with different `.subforge-tm/` paths.
    let tm_anchor = input_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    let cache = if opts.no_cache {
        None
    } else {
        Some(cache::StepCache::new(&cfg.cache_dir()))
    };

    // Step 1: Transcribe (always lands at <out_dir>/<stem>.srt as a working file)
    log_info!("[1/3] Transcribing...");
    let stage_started = Instant::now();
    let srt_path = out_dir.join(format!("{stem}.srt"));
    let key = cache::make_key(&input_path, &cfg.cache_key_transcribe());
    if cache
        .as_ref()
        .and_then(|c| c.restore("transcribe", &key, "srt", &srt_path))
        .is_none()
    {
        transcribe::run_with_config_path(
            input,
            Some(path_str(&srt_path, "transcribe 输出")?),
            &cfg.asr,
            Some("srt"),
            cfg,
            config_path,
        )
        .await?;
        if let Some(c) = &cache
            && let Err(e) = c.store("transcribe", &key, &srt_path)
        {
            log_info!("warning: {e}");
        }
    } else {
        log_info!("  (cache hit)");
    }
    log_info!(
        "{}",
        stage_elapsed_line("[1/3]", "Transcribing", stage_started.elapsed())
    );

    // Step 2: Translate. If `--no-synthesize` AND user gave a file-shaped
    // -o, write directly to that filename. Otherwise write the conventional
    // `<stem>_translated.srt` and let synthesize use it as input.
    log_info!("[2/3] Translating...");
    let stage_started = Instant::now();
    let translated_path = if opts.no_synthesize
        && let Some(p) = &explicit_final
    {
        p.clone()
    } else {
        out_dir.join(util::translated_output_name(&stem, "srt"))
    };
    let key = cache::make_key(&srt_path, &cfg.cache_key_subtitle());
    if cache
        .as_ref()
        .and_then(|c| c.restore("subtitle", &key, "srt", &translated_path))
        .is_none()
    {
        subtitle::run(
            path_str(&srt_path, "subtitle 输入")?,
            Some(path_str(&translated_path, "subtitle 输出")?),
            cfg,
            Some(&tm_anchor),
        )
        .await?;
        if let Some(c) = &cache
            && let Err(e) = c.store("subtitle", &key, &translated_path)
        {
            log_info!("warning: {e}");
        }
    } else {
        log_info!("  (cache hit)");
    }
    log_info!(
        "{}",
        stage_elapsed_line("[2/3]", "Translating", stage_started.elapsed())
    );

    // Step 3: Synthesize (or stop here)
    let did_synthesize = !opts.no_synthesize && util::is_video_file(&input_path);
    let final_path = if !did_synthesize {
        translated_path.clone()
    } else {
        log_info!("[3/3] Synthesizing subtitles...");
        let stage_started = Instant::now();
        let video_out = match &explicit_final {
            Some(p) => p.clone(),
            None => out_dir.join(format!(
                "{}_captioned.{}",
                stem,
                synthesized_default_extension(&opts.synth, &input_path)
            )),
        };
        synthesize::run(
            input,
            path_str(&translated_path, "synthesize 字幕")?,
            Some(path_str(&video_out, "synthesize 输出")?),
            &opts.synth,
            cfg,
        )
        .await?;
        log_info!(
            "{}",
            stage_elapsed_line("[3/3]", "Synthesizing", stage_started.elapsed())
        );
        video_out
    };

    // Optional cleanup: remove intermediates that aren't the final artifact.
    // We never delete files the cache layer might still want — the cache has
    // its own copy under `data_dir`, separate from these working files.
    for path in cleanup_plan(
        opts.keep_intermediate,
        &final_path,
        &srt_path,
        &translated_path,
        did_synthesize,
    ) {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }

    log_info!("{}", total_elapsed_line(total_started.elapsed()));
    Ok(final_path)
}

fn synthesized_default_extension(opts: &synthesize::Options, input: &Path) -> String {
    match opts.mode {
        synthesize::Mode::Soft => "mkv".into(),
        _ => input
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    }
}

fn cleanup_plan(
    keep_intermediate: bool,
    final_path: &Path,
    srt_path: &Path,
    translated_path: &Path,
    did_synthesize: bool,
) -> Vec<PathBuf> {
    if keep_intermediate || did_synthesize {
        return Vec::new();
    }

    [srt_path, translated_path]
        .into_iter()
        .filter(|p| *p != final_path)
        .map(PathBuf::from)
        .collect()
}

fn stage_elapsed_line(step: &str, name: &str, elapsed: Duration) -> String {
    format!("{step} {name} completed in {}", format_elapsed(elapsed))
}

fn total_elapsed_line(elapsed: Duration) -> String {
    format!("Total elapsed: {}", format_elapsed(elapsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_output_no_arg_uses_input_parent() {
        let (dir, final_) = resolve_output(Path::new("/videos/x.mp4"), None);
        assert_eq!(dir, PathBuf::from("/videos"));
        assert!(final_.is_none());
    }

    #[test]
    fn elapsed_lines_are_human_readable() {
        assert_eq!(
            stage_elapsed_line("[1/3]", "Transcribing", std::time::Duration::from_secs(65)),
            "[1/3] Transcribing completed in 1m 5s"
        );
        assert_eq!(
            total_elapsed_line(std::time::Duration::from_secs(3661)),
            "Total elapsed: 1h 1m 1s"
        );
    }

    #[test]
    fn resolve_output_trailing_slash_is_directory() {
        let (dir, final_) = resolve_output(Path::new("/videos/x.mp4"), Some("/tmp/out/"));
        assert_eq!(dir, PathBuf::from("/tmp/out/"));
        assert!(final_.is_none());
    }

    #[test]
    fn resolve_output_file_with_extension_is_final_path() {
        // The bug we're fixing: -o /tmp/out.srt used to create /tmp/out.srt as
        // a directory. Now it's recognized as the final filename.
        let (dir, final_) = resolve_output(Path::new("/videos/x.mp4"), Some("/tmp/out.srt"));
        assert_eq!(dir, PathBuf::from("/tmp"));
        assert_eq!(final_, Some(PathBuf::from("/tmp/out.srt")));
    }

    #[test]
    fn resolve_output_file_in_cwd() {
        let (dir, final_) = resolve_output(Path::new("/videos/x.mp4"), Some("out.mp4"));
        assert_eq!(dir, PathBuf::from("."));
        assert_eq!(final_, Some(PathBuf::from("out.mp4")));
    }

    #[test]
    fn resolve_output_dirname_without_extension_is_directory() {
        let (dir, final_) = resolve_output(Path::new("/videos/x.mp4"), Some("/tmp/outputs"));
        assert_eq!(dir, PathBuf::from("/tmp/outputs"));
        assert!(final_.is_none());
    }

    #[test]
    fn resolve_output_existing_directory_stays_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, final_) = resolve_output(
            Path::new("/videos/x.mp4"),
            Some(tmp.path().to_str().unwrap()),
        );
        assert_eq!(dir, tmp.path());
        assert!(final_.is_none());
    }

    #[test]
    fn synthesized_outputs_keep_source_and_bilingual_subtitles() {
        let source = Path::new("/videos/demo.srt");
        let bilingual = Path::new("/videos/demo_translated.srt");
        let final_video = Path::new("/videos/demo_captioned.mp4");

        let cleanup = cleanup_plan(false, final_video, source, bilingual, true);

        assert!(cleanup.is_empty());
    }

    #[test]
    fn path_str_accepts_utf8_path() {
        let p = Path::new("/tmp/中文/视频.mp4");
        assert_eq!(path_str(p, "test").unwrap(), "/tmp/中文/视频.mp4");
    }

    #[cfg(unix)]
    #[test]
    fn path_str_rejects_non_utf8_with_friendly_message() {
        // On Unix, paths are bytes; construct a path that's not valid UTF-8.
        use std::os::unix::ffi::OsStrExt;
        let bad = std::ffi::OsStr::from_bytes(&[0xff, 0xfe, 0xfd]);
        let p = Path::new(bad);
        let err = path_str(p, "test").unwrap_err();
        assert!(err.contains("UTF-8"), "expected UTF-8 mention in: {err}");
        assert!(err.contains("test"), "expected label in: {err}");
    }
}
