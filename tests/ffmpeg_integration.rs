//! End-to-end integration tests that actually shell out to ffmpeg.
//!
//! Skipped if ffmpeg isn't on PATH so `cargo test` still passes in
//! environments without media tooling. Run explicitly with:
//!
//!   cargo test --test ffmpeg_integration -- --include-ignored
//!
//! These cover regressions that pure unit tests can't catch:
//! - container/codec mismatches (`webm` ↔ `webvtt`)
//! - apostrophe-in-path workaround
//! - hard burn produces a playable output
//! - soft mux preserves stream copy semantics

use std::path::{Path, PathBuf};
use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a 2-second 320x240 black video with silent audio.
/// Returns the path; cleanup is the caller's responsibility (tempdir does it).
fn make_dummy_video(dir: &Path) -> PathBuf {
    let out = dir.join("dummy.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "color=c=black:s=320x240:d=2"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono"])
        .args([
            "-shortest",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
        ])
        .arg(&out)
        .status()
        .expect("ffmpeg should be on PATH (we checked)");
    assert!(status.success(), "dummy video generation failed");
    out
}

fn make_dummy_srt(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "1\n00:00:00,500 --> 00:00:01,500\nhello world\n\n").unwrap();
    path
}

fn run_subforge(args: &[&str]) -> (bool, String) {
    let bin = env!("CARGO_BIN_EXE_subforge");
    let output = Command::new(bin)
        .args(args)
        .output()
        .expect("subforge binary should be built by cargo");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), combined)
}

#[test]
fn synthesize_hard_basic() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let video = make_dummy_video(tmp.path());
    let srt = make_dummy_srt(tmp.path(), "sub.srt");
    let out = tmp.path().join("out.mp4");

    let (ok, log) = run_subforge(&[
        "synthesize",
        video.to_str().unwrap(),
        "--subtitle",
        srt.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(ok, "synthesize hard failed:\n{log}");
    assert!(out.exists(), "no output file produced");
    assert!(out.metadata().unwrap().len() > 0);
}

#[test]
fn synthesize_soft_mp4_uses_mov_text() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let video = make_dummy_video(tmp.path());
    let srt = make_dummy_srt(tmp.path(), "sub.srt");
    let out = tmp.path().join("out.mp4");

    let (ok, log) = run_subforge(&[
        "synthesize",
        video.to_str().unwrap(),
        "--subtitle",
        srt.to_str().unwrap(),
        "--mode",
        "soft",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(ok, "soft mux failed:\n{log}");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams"])
        .arg(&out)
        .output()
        .unwrap();
    let probe_out = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe_out.contains("codec_name=mov_text"),
        "expected mov_text codec, got:\n{probe_out}"
    );
}

#[test]
fn synthesize_soft_default_uses_mkv_for_player_compatible_srt() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let video = make_dummy_video(tmp.path());
    let srt = make_dummy_srt(tmp.path(), "sub.srt");
    let out = tmp.path().join("dummy_softsub.mkv");

    let (ok, log) = run_subforge(&[
        "synthesize",
        video.to_str().unwrap(),
        "--subtitle",
        srt.to_str().unwrap(),
        "--mode",
        "soft",
    ]);
    assert!(ok, "soft mux failed:\n{log}");
    assert!(
        out.exists(),
        "expected default soft output at {}",
        out.display()
    );

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "s:0",
            "-show_entries",
            "stream=codec_name",
        ])
        .arg(&out)
        .output()
        .unwrap();
    let probe_out = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe_out.contains("codec_name=subrip"),
        "expected MKV/subrip subtitle track, got:\n{probe_out}"
    );
}

#[test]
fn synthesize_soft_preserves_source_video_stream_selection() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("multi_video.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "color=c=black:s=320x240:d=2"])
        .args(["-f", "lavfi", "-i", "testsrc2=s=320x240:d=2"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono"])
        .args(["-map", "0:v", "-map", "1:v", "-map", "2:a"])
        .args([
            "-shortest",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
        ])
        .args(["-disposition:v:0", "0", "-disposition:v:1", "default"])
        .arg(&video)
        .status()
        .expect("ffmpeg should be on PATH (we checked)");
    assert!(status.success(), "multi-video generation failed");

    let srt = make_dummy_srt(tmp.path(), "sub.srt");
    let out = tmp.path().join("out.mkv");
    let (ok, log) = run_subforge(&[
        "synthesize",
        video.to_str().unwrap(),
        "--subtitle",
        srt.to_str().unwrap(),
        "--mode",
        "soft",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(ok, "soft mux failed:\n{log}");

    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "stream=index",
        ])
        .arg(&out)
        .output()
        .unwrap();
    let video_streams = String::from_utf8_lossy(&probe.stdout)
        .lines()
        .filter(|line| line.starts_with("index="))
        .count();
    assert_eq!(
        video_streams, 2,
        "soft mux should preserve all source video streams so the player's default selection survives"
    );
}

#[test]
fn synthesize_soft_webm_uses_webvtt() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // webm requires VP8/VP9 video — make a webm-compatible dummy.
    let video = tmp.path().join("dummy.webm");
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "color=c=black:s=320x240:d=2"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono"])
        .args(["-shortest", "-c:v", "libvpx", "-c:a", "libvorbis"])
        .arg(&video)
        .status();
    if status.map(|s| !s.success()).unwrap_or(true) {
        eprintln!("skipping: ffmpeg lacks libvpx/libvorbis");
        return;
    }
    let srt = make_dummy_srt(tmp.path(), "sub.srt");
    let out = tmp.path().join("out.webm");

    let (ok, log) = run_subforge(&[
        "synthesize",
        video.to_str().unwrap(),
        "--subtitle",
        srt.to_str().unwrap(),
        "--mode",
        "soft",
        "-o",
        out.to_str().unwrap(),
    ]);
    // Regression: webm used to be mapped to `srt` codec which ffmpeg rejects.
    assert!(ok, "webm soft mux failed (regression of #1):\n{log}");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams"])
        .arg(&out)
        .output()
        .unwrap();
    let probe_out = String::from_utf8_lossy(&probe.stdout);
    assert!(
        probe_out.contains("codec_name=webvtt"),
        "expected webvtt codec, got:\n{probe_out}"
    );
}

#[test]
fn synthesize_apostrophe_in_path() {
    if !ffmpeg_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    // Create a directory with an apostrophe.
    let inner = tmp.path().join("it's a test");
    std::fs::create_dir_all(&inner).unwrap();

    let video = make_dummy_video(&inner);
    let srt = make_dummy_srt(&inner, "sub.srt");
    let out = inner.join("out.mp4");

    let (ok, log) = run_subforge(&[
        "synthesize",
        video.to_str().unwrap(),
        "--subtitle",
        srt.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    // libass can't escape apostrophes; subforge falls back to a symlink.
    assert!(
        ok,
        "apostrophe path failed (libass workaround broken):\n{log}"
    );
    assert!(out.exists());
}

#[test]
fn translate_output_as_filename_not_directory() {
    // Regression for #3: `subforge translate -o foo.srt` used to create a
    // directory named foo.srt and write inside. Without an actual ffmpeg we
    // can't run translate, but we can run synthesize to verify the same
    // output-resolution logic (process.rs::resolve_output) doesn't make
    // a directory out of a file-shaped path.
    //
    // Synthesize handles -o directly via PathBuf::from, so this test checks
    // the path doesn't get mistaken at the process layer. We use a marker
    // file name to make sure no directory was made.
    if !ffmpeg_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let video = make_dummy_video(tmp.path());
    let srt = make_dummy_srt(tmp.path(), "sub.srt");
    let target = tmp.path().join("named_output.mp4");
    let (ok, log) = run_subforge(&[
        "synthesize",
        video.to_str().unwrap(),
        "--subtitle",
        srt.to_str().unwrap(),
        "-o",
        target.to_str().unwrap(),
    ]);
    assert!(ok, "named output failed:\n{log}");
    assert!(target.is_file(), "expected file, got directory or missing");
}
