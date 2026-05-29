use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// A single subtitle segment with timing and text.
#[derive(Debug, Clone)]
pub struct Segment {
    pub index: usize,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "ts", "m4v",
];

pub fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub async fn extract_audio_wav(input: &Path, output: &Path) -> Result<(), String> {
    // Capture stderr so a failure surfaces ffmpeg's actual reason instead of
    // a bare "audio extraction failed" with no diagnostics.
    let output_res = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(input)
        .args(["-vn", "-acodec", "pcm_s16le", "-ar", "16000", "-ac", "1"])
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("ffmpeg not found: {e}\n\n{}", ffmpeg_install_hint()))?;
    if !output_res.status.success() {
        let stderr = String::from_utf8_lossy(&output_res.stderr);
        let tail: String = stderr
            .lines()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("ffmpeg audio extraction failed:\n{tail}"));
    }
    Ok(())
}

/// OS-specific, copy-pasteable install instructions for ffmpeg.
///
/// Used in every "ffmpeg not found" error path and by `subforge doctor` so
/// the user gets an actionable command instead of a bare failure.
pub fn ffmpeg_install_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "ffmpeg 未安装。安装方式（任选其一）：\n  \
         winget install Gyan.FFmpeg\n  \
         choco install ffmpeg\n  \
         scoop install ffmpeg\n\
         或从 https://www.gyan.dev/ffmpeg/builds/ 下载后将 bin 目录加入 PATH"
    } else if cfg!(target_os = "macos") {
        "ffmpeg 未安装。安装方式：\n  brew install ffmpeg"
    } else {
        "ffmpeg 未安装。安装方式：\n  \
         Debian/Ubuntu:  sudo apt install ffmpeg\n  \
         Fedora:         sudo dnf install ffmpeg\n  \
         Arch:           sudo pacman -S ffmpeg"
    }
}

/// RAII guard for a tmp file path. Drop deletes the file (best-effort).
///
/// # Why this exists
///
/// The transcribe path used to write WAV / Python script / whisper-cpp SRT
/// files into `/tmp` and only delete them at the *end* of the happy path.
/// Any `?` failure mid-pipeline leaked the file forever. With `TempFile`,
/// the cleanup happens whenever the holding scope exits — successful return,
/// `?`-propagated error, or panic.
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Parse SRT content into Segment structs
pub fn parse_srt(content: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut lines = content.lines().peekable();
    let mut index = 0;

    while lines.peek().is_some() {
        // Skip empty lines
        while lines.peek().map(|l| l.trim().is_empty()).unwrap_or(false) {
            lines.next();
        }
        // Index line
        if let Some(line) = lines.next() {
            if line.trim().parse::<usize>().is_err() {
                continue;
            }
            index += 1;
        } else {
            break;
        }
        // Timestamp line
        let Some(ts_line) = lines.next() else { break };
        let Some((start, end)) = parse_srt_timestamps(ts_line) else {
            continue;
        };
        // Text lines
        let mut text = String::new();
        while let Some(line) = lines.peek() {
            if line.trim().is_empty() {
                break;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(lines.next().unwrap());
        }
        segments.push(Segment {
            index,
            start_ms: start,
            end_ms: end,
            text,
        });
    }
    segments
}

fn parse_srt_timestamps(line: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = line.split("-->").collect();
    if parts.len() != 2 {
        return None;
    }
    Some((
        parse_srt_time(parts[0].trim())?,
        parse_srt_time(parts[1].trim())?,
    ))
}

fn parse_srt_time(s: &str) -> Option<u64> {
    let s = s.replace(',', ".").trim().to_string();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() == 3 {
        // HH:MM:SS.mmm
        let h: u64 = parts[0].trim().parse().ok()?;
        let m: u64 = parts[1].trim().parse().ok()?;
        let sec_parts: Vec<&str> = parts[2].split('.').collect();
        let sec: u64 = sec_parts[0].trim().parse().ok()?;
        let ms: u64 = match sec_parts.get(1) {
            Some(frac) => {
                let frac = frac.trim();
                let val: u64 = frac.parse().ok()?;
                // Normalize to milliseconds (e.g., "5" -> 500, "50" -> 500, "500" -> 500)
                match frac.len() {
                    1 => val * 100,
                    2 => val * 10,
                    3 => val,
                    _ => val / 10u64.pow(frac.len() as u32 - 3),
                }
            }
            None => 0,
        };
        Some(h * 3600000 + m * 60000 + sec * 1000 + ms)
    } else {
        // SS.mm (decimal seconds) — used by faster-whisper
        let secs: f64 = s.parse().ok()?;
        Some((secs * 1000.0) as u64)
    }
}

/// Format milliseconds as `HH:MM:SS,mmm` per the SRT spec.
///
/// SRT canonically uses two-digit hours, but most modern parsers (mpv, VLC,
/// libass, ffmpeg) accept three-digit hours too. We pad to **at least** two
/// digits (so 1h is `01`, not `1`), and let h≥100 widen naturally — clamping
/// to 99 would corrupt the timeline of recordings longer than a hundred hours
/// (rare, but happens for archival jobs). If your downstream parser is
/// strict, split the source first.
pub fn format_srt_time(ms: u64) -> String {
    let h = ms / 3600000;
    let m = (ms % 3600000) / 60000;
    let s = (ms % 60000) / 1000;
    let milli = ms % 1000;
    format!("{:02}:{:02}:{:02},{:03}", h, m, s, milli)
}

pub fn render_srt(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|s| {
            format!(
                "{}\n{} --> {}\n{}\n",
                s.index,
                format_srt_time(s.start_ms),
                format_srt_time(s.end_ms),
                s.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read a subtitle file with encoding sniffing.
///
/// 1. Try UTF-8 (with or without BOM — `from_utf8` accepts both forms; we
///    strip a BOM if present after decoding).
/// 2. Fall back to a curated list of legacy CJK encodings, picking whichever
///    decoded without "replacement characters". This covers the realistic
///    99% of legacy subtitle files (Simplified-Chinese GBK, Traditional Big5,
///    Japanese Shift-JIS) which the previous GBK-only fallback silently
///    mojibake'd.
pub fn read_subtitle_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;

    // UTF-16 BOMs come from Windows tools that "Save as Unicode" (Notepad,
    // older Office). Detect by leading bytes and decode with the right
    // codec so users don't see a wall of replacement chars.
    if let Some(decoded) = decode_utf16_bom(&bytes) {
        return Ok(decoded);
    }

    if let Ok(s) = std::str::from_utf8(&bytes) {
        return Ok(strip_bom(s).to_string());
    }
    Ok(decode_legacy_cjk(&bytes))
}

/// Detect UTF-16 LE / BE byte-order marks and decode accordingly.
/// Returns None if the input doesn't have a UTF-16 BOM.
fn decode_utf16_bom(bytes: &[u8]) -> Option<String> {
    use encoding_rs::{UTF_16BE, UTF_16LE};
    if bytes.len() >= 2 {
        match (bytes[0], bytes[1]) {
            // UTF-16 LE BOM
            (0xFF, 0xFE) => {
                let (decoded, _, _) = UTF_16LE.decode(bytes);
                return Some(strip_bom(&decoded).to_string());
            }
            // UTF-16 BE BOM
            (0xFE, 0xFF) => {
                let (decoded, _, _) = UTF_16BE.decode(bytes);
                return Some(strip_bom(&decoded).to_string());
            }
            _ => {}
        }
    }
    None
}

fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// Try a sequence of legacy encodings and pick the one with the fewest U+FFFD
/// replacement characters. Ties broken by encoding order (GBK preferred since
/// it's the most common in zh-CN content).
fn decode_legacy_cjk(bytes: &[u8]) -> String {
    use encoding_rs::{BIG5, GBK, SHIFT_JIS, WINDOWS_1252};

    let candidates = [GBK, BIG5, SHIFT_JIS, WINDOWS_1252];
    let mut best = None;
    let mut best_score = usize::MAX;
    for enc in candidates {
        let (decoded, _, had_errors) = enc.decode(bytes);
        let replacements = decoded.matches('\u{FFFD}').count();
        // Strong penalty for had_errors; replacements are also bad.
        let score = if had_errors {
            replacements + 1000
        } else {
            replacements
        };
        if score < best_score {
            best_score = score;
            best = Some(decoded.into_owned());
            if score == 0 {
                break;
            }
        }
    }
    best.unwrap_or_default()
}

/// Compute output filename (without parent dir), avoiding `_translated_translated.srt`
/// when the input is already a translated file.
pub fn translated_output_name(stem: &str, ext: &str) -> String {
    if stem.ends_with("_translated") {
        format!("{stem}.{ext}")
    } else {
        format!("{stem}_translated.{ext}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_srt_basic() {
        let content = "1\n00:00:01,500 --> 00:00:03,750\nHello world\n\n2\n00:00:04,000 --> 00:00:06,000\nFoo\nBar\n";
        let segments = parse_srt(content);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ms, 1500);
        assert_eq!(segments[0].end_ms, 3750);
        assert_eq!(segments[0].text, "Hello world");
        assert_eq!(segments[1].text, "Foo\nBar");
    }

    #[test]
    fn parse_srt_handles_short_fractional_seconds() {
        let content = "1\n00:00:01,5 --> 00:00:02,5\nx\n";
        let segments = parse_srt(content);
        assert_eq!(segments[0].start_ms, 1500);
        assert_eq!(segments[0].end_ms, 2500);
    }

    #[test]
    fn parse_srt_empty_returns_empty() {
        assert!(parse_srt("").is_empty());
        assert!(parse_srt("   \n  \n").is_empty());
    }

    #[test]
    fn parse_srt_skips_garbage_blocks() {
        let content = "not a number\n00:00:01,000 --> 00:00:02,000\nbroken\n\n1\n00:00:01,000 --> 00:00:02,000\nokay\n";
        let segments = parse_srt(content);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "okay");
    }

    #[test]
    fn parse_srt_handles_crlf_line_endings() {
        // Windows-saved SRT uses CRLF. lines() iterator handles this
        // automatically but we want a regression test.
        let content = "1\r\n00:00:01,000 --> 00:00:02,000\r\nHello\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nWorld\r\n";
        let segs = parse_srt(content);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "Hello");
        assert_eq!(segs[1].text, "World");
    }

    #[test]
    fn parse_srt_handles_multiline_cue_text() {
        let content = "1\n00:00:01,000 --> 00:00:02,000\nLine 1\nLine 2\nLine 3\n";
        let segs = parse_srt(content);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "Line 1\nLine 2\nLine 3");
    }

    #[test]
    fn parse_srt_renumbers_indices_in_order() {
        // SRT spec says indices start at 1, but real files have gaps/dupes.
        // We always renumber 1, 2, 3 on output so the result is canonical.
        let content =
            "5\n00:00:01,000 --> 00:00:02,000\nA\n\n7\n00:00:03,000 --> 00:00:04,000\nB\n";
        let segs = parse_srt(content);
        assert_eq!(segs[0].index, 1);
        assert_eq!(segs[1].index, 2);
    }

    #[test]
    fn parse_srt_time_normalizes_short_fractional() {
        // .5 should mean 500ms, not 5ms.
        assert_eq!(parse_srt_time("00:00:01,5").unwrap(), 1500);
        // .50 should also mean 500ms.
        assert_eq!(parse_srt_time("00:00:01,50").unwrap(), 1500);
        // .500 is the normal case.
        assert_eq!(parse_srt_time("00:00:01,500").unwrap(), 1500);
        // .5000 should clamp to ms granularity (drop excess).
        assert_eq!(parse_srt_time("00:00:01,5000").unwrap(), 1500);
    }

    #[test]
    fn parse_srt_time_rejects_negative() {
        // Defensive: timestamps in SRT are unsigned. A leading minus
        // should be a parse failure, not silent zero.
        assert!(parse_srt_time("-00:00:01,000").is_none());
    }

    #[test]
    fn render_srt_roundtrip() {
        let original = vec![
            Segment {
                index: 1,
                start_ms: 1500,
                end_ms: 3750,
                text: "Hello world".to_string(),
            },
            Segment {
                index: 2,
                start_ms: 4000,
                end_ms: 6000,
                text: "Foo\nBar".to_string(),
            },
        ];
        let rendered = render_srt(&original);
        let parsed = parse_srt(&rendered);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].start_ms, 1500);
        assert_eq!(parsed[0].end_ms, 3750);
        assert_eq!(parsed[0].text, "Hello world");
        assert_eq!(parsed[1].text, "Foo\nBar");
    }

    #[test]
    fn format_srt_time_pads_zeros() {
        assert_eq!(format_srt_time(0), "00:00:00,000");
        assert_eq!(format_srt_time(1500), "00:00:01,500");
        assert_eq!(format_srt_time(3661500), "01:01:01,500");
    }

    #[test]
    fn format_srt_time_widens_for_long_durations() {
        // 100 hours: hour field grows to 3 digits. Documented behavior; most
        // modern parsers accept it.
        let ms = 100u64 * 3600000;
        assert_eq!(format_srt_time(ms), "100:00:00,000");
    }

    #[test]
    fn is_video_file_detects_extensions() {
        assert!(is_video_file(Path::new("a.mp4")));
        assert!(is_video_file(Path::new("a.MKV")));
        assert!(!is_video_file(Path::new("a.srt")));
        assert!(!is_video_file(Path::new("a")));
    }

    #[test]
    fn translated_output_name_avoids_duplicate_suffix() {
        assert_eq!(
            translated_output_name("video", "srt"),
            "video_translated.srt"
        );
        assert_eq!(
            translated_output_name("video_translated", "srt"),
            "video_translated.srt"
        );
        assert_eq!(translated_output_name("v", "vtt"), "v_translated.vtt");
    }

    #[test]
    fn temp_file_drops_clean_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scratch.tmp");
        std::fs::write(&path, b"x").unwrap();
        assert!(path.exists());
        {
            let _g = TempFile::new(&path);
        }
        assert!(!path.exists());
    }

    #[test]
    fn temp_file_drop_tolerates_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never_existed.tmp");
        // Should not panic on drop even though the file was never created.
        let _g = TempFile::new(&path);
    }

    #[test]
    fn temp_file_guard_set_before_write_cleans_partial_file() {
        // Regression: previously we installed the guard AFTER the write call.
        // If the write succeeded but the function then errored on a later
        // step, drop ran and cleaned up — fine. But if the write itself
        // partially wrote and errored (rare but possible: ENOSPC mid-write),
        // there'd be no guard to clean it.
        // Now we install the guard FIRST. Verify a path that was guarded
        // but never actually written to causes no issues, and a path that
        // had a partial write on it still gets reaped.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.tmp");
        {
            // Install guard BEFORE write.
            let _g = TempFile::new(&path);
            // Simulate partial write.
            std::fs::write(&path, b"AB").unwrap();
            assert!(path.exists());
        }
        assert!(
            !path.exists(),
            "guard installed before write should still clean partial file"
        );
    }
    #[test]
    fn read_subtitle_strips_utf8_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.srt");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello");
        std::fs::write(&path, &bytes).unwrap();
        let content = read_subtitle_file(&path).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn read_subtitle_decodes_gbk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.srt");
        // 你好 in GBK
        let bytes = [0xC4, 0xE3, 0xBA, 0xC3];
        std::fs::write(&path, bytes).unwrap();
        let content = read_subtitle_file(&path).unwrap();
        assert_eq!(content, "你好");
    }

    #[test]
    fn read_subtitle_decodes_big5() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.srt");
        // 妳好 in Big5
        let bytes = [0xA7, 0xB0, 0xA6, 0x6E];
        std::fs::write(&path, bytes).unwrap();
        let content = read_subtitle_file(&path).unwrap();
        // Best decoder wins; just verify it didn't return mojibake/garbage.
        assert!(!content.is_empty());
        assert!(!content.contains('\u{FFFD}'));
    }

    #[test]
    fn read_subtitle_decodes_utf16_le_bom() {
        // Notepad's "Save as Unicode" produces UTF-16 LE with BOM. We must
        // detect this rather than falling through to CJK heuristics which
        // would mojibake the content.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.srt");
        // BOM (FF FE) + "你好" in UTF-16 LE (60 4F = U+4F60 little-endian, 7D 59 = U+597D)
        let bytes = vec![0xFF, 0xFE, 0x60, 0x4F, 0x7D, 0x59];
        std::fs::write(&path, &bytes).unwrap();
        let content = read_subtitle_file(&path).unwrap();
        assert_eq!(content, "你好");
    }

    #[test]
    fn read_subtitle_decodes_utf16_be_bom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.srt");
        // BOM (FE FF) + "Hi" in UTF-16 BE (00 48 = 'H', 00 69 = 'i')
        let bytes = vec![0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69];
        std::fs::write(&path, &bytes).unwrap();
        let content = read_subtitle_file(&path).unwrap();
        assert_eq!(content, "Hi");
    }
}
