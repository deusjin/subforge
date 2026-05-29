//! Subtitle synthesis: hard-burn or soft-mux into a video container.
//!
//! # Modes
//!
//! - `Hard`: re-encode the video stream with libass `subtitles=` filter so the
//!   subtitle is permanently rendered into pixels. Lossy (re-encode), slow,
//!   irreversible. Style is fully customizable via `force_style`.
//!
//! - `Soft`: mux the subtitle as a separate stream into the container without
//!   re-encoding video/audio. Stream-copy, near-instant, lossless, the player
//!   can toggle subtitles on/off. Container limits which subtitle codec to
//!   use (mp4 → mov_text, mkv → srt). When no output path is specified, soft
//!   mode defaults to MKV so SRT cues stay as SRT for better player behavior.
//!
//! - `Both`: produce both outputs (named `<stem>_captioned.<ext>` and
//!   `<stem>_softsub.mkv` unless an output path is specified).
//!
//! # Apostrophes in paths
//!
//! ffmpeg/libass cannot escape `'` in subtitle paths regardless of escaping
//! method (trac #7329). When the subtitle path contains `'` we create a
//! tempdir symlink with a safe name and feed that to ffmpeg instead. The
//! symlink is RAII-cleaned via [`crate::util::TempFile`].

use crate::config::Config;
use crate::util::TempFile;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Caller-supplied knobs. Built by main.rs from CLI args.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub mode: Mode,

    // Style (hard mode)
    pub font: Option<String>,
    pub font_size: Option<u32>,
    pub font_color: Option<String>,    // RRGGBB
    pub outline_color: Option<String>, // RRGGBB
    pub outline_width: Option<u32>,
    pub position: Option<Position>,
    pub margin_v: Option<u32>,
    pub style_raw: Option<String>, // overrides everything above

    // Encoding (hard mode)
    pub encoder: Option<Encoder>,
    pub crf: Option<u8>,
    pub preset: Option<String>,
    pub max_bitrate: Option<String>,
    /// Target subtitle width as percent of video width (1-100). When set,
    /// libass `MarginL`/`MarginR` are derived to keep subtitles within
    /// that fraction of the screen. Hard mode only — soft mux delegates to
    /// the player's renderer.
    pub width_ratio_percent: Option<u8>,

    // Range
    pub ss: Option<String>,
    pub duration: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Hard,
    Soft,
    Both,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "hard" => Ok(Mode::Hard),
            "soft" => Ok(Mode::Soft),
            "both" => Ok(Mode::Both),
            other => Err(format!("unknown --mode '{other}': use hard / soft / both")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    BottomLeft,
    Bottom,
    BottomRight,
    MiddleLeft,
    Center,
    MiddleRight,
    TopLeft,
    Top,
    TopRight,
}

impl Position {
    /// Accepts simple names (`bottom`, `top`, `center`) and compound
    /// `<vertical>-<horizontal>` forms (`bottom-left`, `top-right`, etc.).
    /// libass calls this "Alignment" and uses numpad layout 1-9.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "bottom" | "bottom-center" => Ok(Position::Bottom),
            "bottom-left" => Ok(Position::BottomLeft),
            "bottom-right" => Ok(Position::BottomRight),
            "center" | "middle" | "middle-center" => Ok(Position::Center),
            "middle-left" | "left" => Ok(Position::MiddleLeft),
            "middle-right" | "right" => Ok(Position::MiddleRight),
            "top" | "top-center" => Ok(Position::Top),
            "top-left" => Ok(Position::TopLeft),
            "top-right" => Ok(Position::TopRight),
            other => Err(format!(
                "unknown --position '{other}': use bottom / top / center / \
                 bottom-left / bottom-right / top-left / top-right / middle-left / middle-right"
            )),
        }
    }
    /// libass Alignment (numpad layout): 1-3 bottom, 4-6 middle, 7-9 top.
    fn alignment(self) -> u8 {
        match self {
            Position::BottomLeft => 1,
            Position::Bottom => 2,
            Position::BottomRight => 3,
            Position::MiddleLeft => 4,
            Position::Center => 5,
            Position::MiddleRight => 6,
            Position::TopLeft => 7,
            Position::Top => 8,
            Position::TopRight => 9,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    X264,
    X265,
    NvencH264,
    NvencHevc,
    QsvH264,
    VideoToolbox,
}

impl Encoder {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "x264" | "libx264" | "h264" => Ok(Encoder::X264),
            "x265" | "libx265" | "hevc" => Ok(Encoder::X265),
            "nvenc" | "nvenc-h264" | "h264_nvenc" => Ok(Encoder::NvencH264),
            "nvenc-hevc" | "hevc_nvenc" => Ok(Encoder::NvencHevc),
            "qsv" | "qsv-h264" | "h264_qsv" => Ok(Encoder::QsvH264),
            "videotoolbox" | "h264_videotoolbox" => Ok(Encoder::VideoToolbox),
            other => Err(format!(
                "unknown --encoder '{other}': x264 / x265 / nvenc / nvenc-hevc / qsv / videotoolbox"
            )),
        }
    }

    fn ffmpeg_name(self) -> &'static str {
        match self {
            Encoder::X264 => "libx264",
            Encoder::X265 => "libx265",
            Encoder::NvencH264 => "h264_nvenc",
            Encoder::NvencHevc => "hevc_nvenc",
            Encoder::QsvH264 => "h264_qsv",
            Encoder::VideoToolbox => "h264_videotoolbox",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

pub async fn run(
    video: &str,
    subtitle: &str,
    output: Option<&str>,
    opts: &Options,
    cfg: &Config,
) -> Result<PathBuf, String> {
    let video_path = PathBuf::from(video);
    let subtitle_path = PathBuf::from(subtitle);
    if !video_path.exists() {
        return Err(format!("video not found: {video}"));
    }
    if !subtitle_path.exists() {
        return Err(format!("subtitle not found: {subtitle}"));
    }
    validate_style_raw(opts.style_raw.as_deref())?;

    match opts.mode {
        Mode::Hard => burn_hard(&video_path, &subtitle_path, output, opts, cfg).await,
        Mode::Soft => mux_soft(&video_path, &subtitle_path, output, opts, cfg).await,
        Mode::Both => {
            // Hard first (slow), then soft (instant). Both files survive.
            // If the user gave -o, derive the soft path by injecting `_softsub`
            // before its extension. Otherwise derive from the video stem.
            let hard_out = burn_hard(&video_path, &subtitle_path, output, opts, cfg).await?;
            let soft_target = match output {
                Some(o) => insert_marker_before_ext(Path::new(o), "_softsub"),
                None => derive_output_path(&video_path, None, "_softsub", Some("mkv")),
            };
            let soft_override = soft_target.to_string_lossy().into_owned();
            let _soft_out =
                mux_soft(&video_path, &subtitle_path, Some(&soft_override), opts, cfg).await?;
            Ok(hard_out)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Hard burn
// ─────────────────────────────────────────────────────────────────────────────

async fn burn_hard(
    video: &Path,
    subtitle: &Path,
    output: Option<&str>,
    opts: &Options,
    cfg: &Config,
) -> Result<PathBuf, String> {
    let out_path = match output {
        Some(o) => PathBuf::from(o),
        None => derive_output_path(video, None, "_captioned", None),
    };

    // When width_ratio is set, libass's default WrapStyle=0 fights us — it
    // balances line widths even when more horizontal space is available, so
    // simply shrinking MarginL/R via force_style does nothing visible.
    // Generate a real ASS file with WrapStyle=1 and the requested margins.
    // Otherwise stay on the SRT fast path.
    let use_ass_path = opts.width_ratio_percent.is_some_and(|r| r > 0 && r < 100);

    let (effective_sub, _link_guard, _ass_guard) = if use_ass_path {
        let (w, h) = probe_video_dimensions(video).await.unwrap_or((1280, 720));
        let srt_text = std::fs::read_to_string(subtitle).map_err(|e| format!("read srt: {e}"))?;
        let ass = srt_to_ass(&srt_text, w, h, opts);
        let ass_path =
            std::env::temp_dir().join(format!("subforge_burn_{}.ass", std::process::id()));
        // Install the cleanup guard BEFORE the write so a partial file
        // from a failing write also gets removed on drop.
        let g = TempFile::new(&ass_path);
        std::fs::write(&ass_path, ass).map_err(|e| format!("write ass: {e}"))?;
        // Apostrophe workaround still needed (the ass_path itself shouldn't
        // contain `'` since it's our temp file, but be consistent).
        let (sub, link) = ensure_safe_subtitle_path(&ass_path)?;
        (sub, link, Some(g))
    } else {
        let (sub, link) = ensure_safe_subtitle_path(subtitle)?;
        (sub, link, None)
    };

    let escaped = escape_subtitles_path(&effective_sub);
    let force_style = build_force_style(opts);
    let filter = if force_style.is_empty() {
        format!("subtitles={escaped}")
    } else {
        format!("subtitles={escaped}:force_style='{force_style}'")
    };

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y");
    apply_ffmpeg_loglevel(&mut cmd);
    // Fast seek before -i is decoder-level, faster but slightly less accurate
    // on keyframe boundaries. For preview/clip use cases it's the right
    // trade-off; full transcodes typically don't pass --ss anyway.
    if let Some(ss) = &opts.ss {
        cmd.args(["-ss", ss]);
    }
    cmd.args(["-i"]).arg(video);
    if let Some(t) = &opts.duration {
        cmd.args(["-t", t]);
    }
    cmd.args(["-vf", &filter]);

    // Video codec + quality
    add_encode_args(&mut cmd, opts);
    if matches!(
        opts.encoder.unwrap_or(Encoder::X264),
        Encoder::NvencH264 | Encoder::NvencHevc
    ) {
        cmd.envs(crate::gpu::cuda_env(cfg));
    }

    cmd.args(["-c:a", "copy"]);
    cmd.arg(&out_path);
    cmd.stdout(Stdio::null()).stderr(Stdio::inherit());
    cmd.kill_on_drop(true);

    let status = cmd.status().await.map_err(|e| {
        format!(
            "ffmpeg not found: {e}\n\n{}",
            crate::util::ffmpeg_install_hint()
        )
    })?;
    if !status.success() {
        return Err("ffmpeg subtitle burning failed".into());
    }
    Ok(out_path)
}

fn add_encode_args(cmd: &mut Command, opts: &Options) {
    let encoder = opts.encoder.unwrap_or(Encoder::X264);
    cmd.args(["-c:v", encoder.ffmpeg_name()]);

    let crf = opts.crf;
    let preset = opts.preset.as_deref();

    match encoder {
        Encoder::X264 | Encoder::X265 => {
            if let Some(c) = crf {
                cmd.args(["-crf", &c.to_string()]);
            }
            if let Some(p) = preset {
                cmd.args(["-preset", p]);
            }
        }
        Encoder::NvencH264 | Encoder::NvencHevc => {
            // NVENC: use VBR + cq for "constant quality" mode.
            if let Some(c) = crf {
                cmd.args(["-rc", "vbr", "-cq", &c.to_string()]);
            }
            if let Some(p) = preset {
                cmd.args(["-preset", map_nvenc_preset(p)]);
            }
        }
        Encoder::QsvH264 => {
            if let Some(c) = crf {
                cmd.args(["-global_quality", &c.to_string()]);
            }
            if let Some(p) = preset {
                cmd.args(["-preset", p]);
            }
        }
        Encoder::VideoToolbox => {
            // VideoToolbox doesn't accept CRF; map to -q:v (1-100, higher=better).
            // Approximate: 100 - crf*2, clamped 1..=100.
            if let Some(c) = crf {
                let q = 100i32.saturating_sub(c as i32 * 2).clamp(1, 100);
                cmd.args(["-q:v", &q.to_string()]);
            }
            // No preset support.
        }
    }

    if let Some(rate) = &opts.max_bitrate {
        cmd.args(["-maxrate", rate]);
        // bufsize 2x is the standard recommendation for VBV-constrained encodes.
        let bufsize = double_bitrate(rate).unwrap_or_else(|| rate.clone());
        cmd.args(["-bufsize", &bufsize]);
    }
}

/// Translate a libx264-style preset name to NVENC's p1..p7.
fn map_nvenc_preset(p: &str) -> &'static str {
    match p.to_ascii_lowercase().as_str() {
        "veryfast" | "ultrafast" => "p1",
        "fast" | "superfast" => "p3",
        "medium" => "p4",
        "slow" => "p6",
        "veryslow" | "slower" => "p7",
        _ => "p4",
    }
}

/// Multiply a bitrate string by 2. Supports `123`, `5M`, `8000k`, `2.5M`.
/// Returns None on parse failure (caller falls back to the raw value).
fn double_bitrate(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let (num_part, suffix): (&str, &str) = if let Some(stripped) = trimmed.strip_suffix(['M', 'm'])
    {
        (stripped, "M")
    } else if let Some(stripped) = trimmed.strip_suffix(['K', 'k']) {
        (stripped, "k")
    } else {
        (trimmed, "")
    };
    let n: f64 = num_part.parse().ok()?;
    let doubled = n * 2.0;
    Some(if doubled.fract() == 0.0 {
        format!("{}{}", doubled as u64, suffix)
    } else {
        format!("{doubled}{suffix}")
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Soft mux
// ─────────────────────────────────────────────────────────────────────────────

async fn mux_soft(
    video: &Path,
    subtitle: &Path,
    output: Option<&str>,
    opts: &Options,
    cfg: &Config,
) -> Result<PathBuf, String> {
    // Default to MKV so SRT stays SRT. MP4 requires mov_text, which is less
    // reliable for multiline/styled subtitles in common desktop players.
    let out_path = match output {
        Some(o) => PathBuf::from(o),
        None => derive_output_path(video, None, "_softsub", Some("mkv")),
    };
    let ext = out_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mkv")
        .to_ascii_lowercase();
    let sub_codec = match ext.as_str() {
        "mp4" | "m4v" | "mov" => "mov_text",
        "mkv" => "srt",
        // webm only supports webvtt — using srt corrupts the container.
        "webm" => "webvtt",
        other => {
            return Err(format!(
                "soft mux: unsupported output container '.{other}' (use .mp4 / .mkv / .webm)"
            ));
        }
    };

    // Apostrophe-in-path workaround: ffmpeg's `-i` arg currently passes
    // through unmolested, so soft mux tolerates `'` in practice — but this
    // depends on ffmpeg's input parser staying simple. To stay robust against
    // future libavfilter-style escaping creeping into `-i`, route through the
    // same symlink that hard mode uses.
    let (effective_sub, _link_guard) = ensure_safe_subtitle_path(subtitle)?;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y");
    apply_ffmpeg_loglevel(&mut cmd);
    if let Some(ss) = &opts.ss {
        // Both inputs need the same -ss to keep subtitle timestamps aligned
        // with the cropped video. ffmpeg's input-side -ss shifts subtitle
        // timestamps to start at 0 just like it does for video.
        cmd.args(["-ss", ss]);
    }
    cmd.args(["-i"]).arg(video);
    if let Some(ss) = &opts.ss {
        cmd.args(["-ss", ss]);
    }
    cmd.args(["-i"]).arg(&effective_sub);
    if let Some(t) = &opts.duration {
        cmd.args(["-t", t]);
    }

    // Stream-copy all source video/audio tracks so the container keeps the
    // same playable stream set and dispositions. Mapping only 0:v:0 can pick
    // a cover/placeholder stream on some MP4s, which shows up as black video.
    // Existing subtitle streams are intentionally dropped; input 1 is the new
    // generated subtitle track.
    cmd.args(["-map", "0:v?", "-map", "0:a?", "-map", "1:s:0"]);
    cmd.arg("-c").arg("copy");
    cmd.args(["-c:s", sub_codec]);

    // Tag the language so players display it in the subtitle picker, and
    // mark it default so it's the one that auto-shows.
    let lang = ffmpeg_lang_tag(&cfg.target_language);
    cmd.args(["-metadata:s:s:0", &format!("language={lang}")]);
    cmd.args(["-disposition:s:0", "default"]);

    cmd.arg(&out_path);
    cmd.stdout(Stdio::null()).stderr(Stdio::inherit());
    cmd.kill_on_drop(true);

    let status = cmd.status().await.map_err(|e| {
        format!(
            "ffmpeg not found: {e}\n\n{}",
            crate::util::ffmpeg_install_hint()
        )
    })?;
    if !status.success() {
        let hint = if matches!(ext.as_str(), "webm") {
            "\nNote: webm soft mux requires the source video to already be VP8/VP9/AV1 + Vorbis/Opus. \
             For h264/aac sources, use --mode hard or output to .mkv / .mp4."
        } else {
            ""
        };
        return Err(format!("ffmpeg subtitle mux failed{hint}"));
    }
    Ok(out_path)
}

/// Map our `target_language` (`zh-Hans`, `en`, `ja`, ...) onto an ISO-639-2
/// 3-letter code suitable for ffmpeg metadata. Falls back to the raw code if
/// unknown — ffmpeg accepts both 2- and 3-letter forms.
fn ffmpeg_lang_tag(target: &str) -> String {
    match target.to_ascii_lowercase().as_str() {
        "zh-hans" | "zh-cn" | "zh" | "chinese" => "zho".into(),
        "zh-hant" | "zh-tw" => "zho".into(),
        "en" | "english" => "eng".into(),
        "ja" | "japanese" => "jpn".into(),
        "ko" | "korean" => "kor".into(),
        "fr" | "french" => "fra".into(),
        "de" | "german" => "deu".into(),
        "es" | "spanish" => "spa".into(),
        "ru" | "russian" => "rus".into(),
        "pt" | "portuguese" => "por".into(),
        other => other.into(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Style / force_style construction
// ─────────────────────────────────────────────────────────────────────────────

/// Compose a libass `force_style` clause from the high-level Options.
/// Returns empty when no style fields are set (so the filter stays minimal).
///
/// libass parses `force_style` as comma-separated `Key=Value` pairs. Any
/// commas inside a value (e.g. font names like "Arial, Black") would split
/// the value and silently drop the second half. We sanitize each value to
/// strip metacharacters that libass has no escape syntax for.
fn build_force_style(opts: &Options) -> String {
    if let Some(raw) = &opts.style_raw {
        // User asked to bypass our wrapper — pass through verbatim.
        return raw.clone();
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(font) = &opts.font {
        parts.push(format!("FontName={}", sanitize_style_value(font)));
    }
    if let Some(size) = opts.font_size {
        parts.push(format!("FontSize={size}"));
    }
    if let Some(c) = &opts.font_color
        && let Some(ass_color) = rrggbb_to_ass(c)
    {
        parts.push(format!("PrimaryColour={ass_color}"));
    }
    if let Some(c) = &opts.outline_color
        && let Some(ass_color) = rrggbb_to_ass(c)
    {
        parts.push(format!("OutlineColour={ass_color}"));
    }
    if let Some(w) = opts.outline_width {
        parts.push(format!("Outline={w}"));
    }
    if let Some(p) = opts.position {
        parts.push(format!("Alignment={}", p.alignment()));
    }
    if let Some(m) = opts.margin_v {
        parts.push(format!("MarginV={m}"));
    }
    // Width ratio: derive MarginL/MarginR from the requested screen-width
    // percentage. libass projects MarginL/R from script units to actual
    // pixels via PlayResX (= 384 for SRT), so the formula gives a value
    // that's resolution-independent: ratio=90 → margin=19 each side ≈ 5%.
    if let Some(ratio) = opts.width_ratio_percent
        && ratio > 0
        && ratio <= 100
    {
        // PlayResX for SRT is 384 in libass. (100 - ratio)% of that, halved.
        let margin = ((100 - ratio as u32) * 384) / 200;
        parts.push(format!("MarginL={margin}"));
        parts.push(format!("MarginR={margin}"));
    }

    parts.join(",")
}

/// libass force_style values can't contain `,` (key/value separator) or `'`
/// (the surrounding quote). Neither has an official escape syntax, so we
/// substitute with the closest visually-equivalent character. This matters
/// most for font names like "Arial, Black" or "It's a font" which would
/// otherwise corrupt the entire force_style clause.
fn sanitize_style_value(s: &str) -> String {
    s.replace([','], " ").replace('\'', "")
}

/// Validate raw `--style` input before it reaches ffmpeg. The wrapped
/// fields (`--font`, `--font-size`, ...) go through `sanitize_style_value`
/// first, but `--style` is intentionally pass-through. Reject characters
/// that would corrupt the filter graph quoting:
///
/// - `'` cannot appear inside the surrounding `force_style='...'` (libass
///   has no escape for it).
/// - `\n` / `\r` would terminate the filter argument and inject a new
///   one — high-risk for command injection if the value came from
///   somewhere besides the user's own keyboard.
///
/// Other filter-graph metacharacters (`:`, `;`, `[`, `]`) are fine inside
/// the single-quoted region per ffmpeg's filter graph syntax.
fn validate_style_raw(s: Option<&str>) -> Result<(), String> {
    let Some(raw) = s else { return Ok(()) };
    if let Some(c) = raw.chars().find(|c| matches!(c, '\'' | '\n' | '\r')) {
        return Err(format!(
            "--style 包含非法字符 {c:?}（不能含有 ' / 换行 / 回车）；\n\
             如需在样式值里写撇号，请改用 --font / --font-color 等独立参数。\n\
             收到的值: {raw:?}"
        ));
    }
    Ok(())
}

/// Convert `RRGGBB` (or `#RRGGBB`) into libass color literal `&H00BBGGRR&`.
/// Returns None for malformed input — the caller drops the field rather
/// than corrupting the entire force_style clause.
fn rrggbb_to_ass(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    // libass: &HAABBGGRR& with alpha 00 = fully opaque.
    Some(format!("&H00{:02X}{:02X}{:02X}&", b, g, r))
}

// ─────────────────────────────────────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────
// SRT → ASS conversion (used when width_ratio is set)
// ─────────────────────────────────────────────────────────────────────────────

/// Probe video width/height via ffprobe. Returns (width, height) in pixels.
/// Falls back to None on probe failure; the caller picks a sane default.
async fn probe_video_dimensions(video: &Path) -> Option<(u32, u32)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
        ])
        .arg(video)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Convert our SRT (with optional `<font color="#RRGGBB">...</font>` tags) into
/// an ASS file with explicit `[Script Info] WrapStyle=1` and resolution-aware
/// margins. The ASS path lets us bypass libass's "balanced wrap" default that
/// otherwise refuses to make full use of available width.
fn srt_to_ass(srt: &str, video_w: u32, video_h: u32, opts: &Options) -> String {
    let cues = crate::util::parse_srt(srt);

    // Margins in actual pixels (PlayResX = video width keeps script units 1:1).
    let ratio = opts.width_ratio_percent.unwrap_or(0);
    let margin_h = if ratio == 0 {
        60
    } else {
        (100 - ratio as u32) * video_w / 200
    };

    // Style fields
    let font_name = opts.font.as_deref().unwrap_or("Arial");
    // Default font size scales with video height when not set.
    let font_size = opts.font_size.unwrap_or((video_h as f32 * 0.045) as u32);
    let outline = opts.outline_width.unwrap_or(2);
    let alignment = opts.position.map(|p| p.alignment()).unwrap_or(2);
    let margin_v = opts.margin_v.unwrap_or((video_h as f32 * 0.04) as u32);
    let primary = opts
        .font_color
        .as_deref()
        .and_then(rrggbb_to_ass_bgr)
        .unwrap_or_else(|| "&H00FFFFFF".into());
    let outline_col = opts
        .outline_color
        .as_deref()
        .and_then(rrggbb_to_ass_bgr)
        .unwrap_or_else(|| "&H00000000".into());

    let mut out = String::with_capacity(srt.len() + 1024);
    out.push_str(&format!(
        "[Script Info]\n\
         ScriptType: v4.00+\n\
         PlayResX: {video_w}\n\
         PlayResY: {video_h}\n\
         WrapStyle: 1\n\
         ScaledBorderAndShadow: yes\n\n",
    ));
    out.push_str(
        "[V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, BackColour, \
         Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, \
         Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
    );
    out.push_str(&format!(
        "Style: Default,{font_name},{font_size},{primary},{outline_col},&H00000000,\
         0,0,0,0,100,100,0,0,1,{outline},0,{alignment},{margin_h},{margin_h},{margin_v},1\n\n",
    ));
    out.push_str(
        "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
    );
    for cue in &cues {
        let text = html_font_to_ass(&cue.text);
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            ass_time(cue.start_ms),
            ass_time(cue.end_ms),
            text
        ));
    }
    out
}

/// Convert RRGGBB hex to ASS color literal `&HAABBGGRR` (alpha 00 = opaque).
fn rrggbb_to_ass_bgr(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(format!("&H00{:02X}{:02X}{:02X}", b, g, r))
}

/// Format milliseconds as ASS time: `H:MM:SS.cc` (centiseconds, single-digit hour).
fn ass_time(ms: u64) -> String {
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1_000;
    let cs = (ms % 1_000) / 10;
    format!("{h}:{m:02}:{s:02}.{cs:02}")
}

/// Translate our SRT-with-`<font>` payload into ASS event text:
/// - `<font color="#RRGGBB">…</font>` → `{\c&Hbbggrr&}…{\r}`
/// - newlines (`\n`) → ASS hard breaks (`\N`)
fn html_font_to_ass(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut chars = s.chars().peekable();
    let mut buf = String::new();
    while let Some(c) = chars.next() {
        buf.push(c);
        // crude state machine: look for `<font color="#XXXXXX">` and `</font>`
        if buf.ends_with("<font color=\"#") || buf.ends_with("<font color='#") {
            // collect 6 hex chars + closing quote + `>`
            let prefix_len = buf.len();
            let hex: String = chars.by_ref().take(6).collect();
            let _quote = chars.next();
            let _gt = chars.next();
            // Trim the open tag start from buf
            buf.truncate(prefix_len - "<font color=\"#".len());
            if let Some(ass) = rrggbb_to_ass_bgr(&hex) {
                buf.push_str(&format!("{{\\c{ass}&}}"));
            }
        } else if buf.ends_with("</font>") {
            let n = buf.len();
            buf.truncate(n - "</font>".len());
            buf.push_str(r"{\r}");
        }
    }
    out.push_str(&buf);
    // Replace literal newlines with ASS hard breaks.
    out.replace('\n', "\\N")
}

// ─────────────────────────────────────────────────────────────────────────────
// Path helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build an output path adjacent to `video`, inserting `marker` before the
/// extension. e.g. `video.mp4` + `_captioned` → `video_captioned.mp4`.
/// `force_ext` lets the caller change the extension (e.g. mp4 → mkv).
fn derive_output_path(
    video: &Path,
    override_path: Option<&str>,
    marker: &str,
    force_ext: Option<&str>,
) -> PathBuf {
    if let Some(o) = override_path {
        return PathBuf::from(o);
    }
    let stem = video.file_stem().unwrap_or_default().to_string_lossy();
    let ext = force_ext.map(String::from).unwrap_or_else(|| {
        video
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    video.with_file_name(format!("{stem}{marker}.{ext}"))
}

/// Insert `marker` before the extension of an existing path. Used to derive
/// the soft-mux output path from the user-specified hard-burn output in
/// `--mode both` (so the two files don't collide).
fn insert_marker_before_ext(path: &Path, marker: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let new_name = if ext.is_empty() {
        format!("{stem}{marker}")
    } else {
        format!("{stem}{marker}.{ext}")
    };
    path.with_file_name(new_name)
}

/// If `subtitle` contains `'` (which libass cannot escape — ffmpeg trac
/// #7329), create a tempfile symlink under a safe name and return its path.
/// Returns the original path otherwise. The TempFile guard cleans the
/// symlink on every drop path.
fn ensure_safe_subtitle_path(subtitle: &Path) -> Result<(PathBuf, Option<TempFile>), String> {
    let s = subtitle.to_string_lossy();
    if !s.contains('\'') {
        return Ok((subtitle.to_path_buf(), None));
    }
    let safe = std::env::temp_dir().join(format!("subforge_sub_{}.srt", std::process::id()));
    // Best-effort cleanup of any stale link from a previous crash.
    let _ = std::fs::remove_file(&safe);
    #[cfg(unix)]
    std::os::unix::fs::symlink(subtitle, &safe).map_err(|e| format!("symlink failed: {e}"))?;
    #[cfg(not(unix))]
    std::fs::copy(subtitle, &safe).map_err(|e| format!("copy failed: {e}"))?;
    let guard = TempFile::new(&safe);
    Ok((safe, Some(guard)))
}

/// Escape a subtitle path for the libavfilter `subtitles=` filter argument.
///
/// Inside single quotes, libavfilter treats most characters literally.
/// Escapes needed: `\` → `\\`, and `:` `;` `[` `]` each prefixed with `\`
/// because they are filter-graph metacharacters that bleed through even
/// inside quotes. Apostrophes can NOT be escaped (libass bug — handled by
/// the symlink workaround above).
fn escape_subtitles_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ':' => out.push_str("\\:"),
            ';' => out.push_str("\\;"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// ffmpeg loglevel adjustment based on subforge verbosity
// ─────────────────────────────────────────────────────────────────────────────

/// Configure ffmpeg's verbosity to match subforge's `-q` / `-v` flags.
/// Normal mode keeps ffmpeg's live `frame=... time=... speed=...` status so
/// long hard-burn jobs visibly move; quiet mode suppresses it.
///
/// Mapping:
/// - subforge Quiet  → ffmpeg `-loglevel error -hide_banner -nostats`
/// - subforge Normal → ffmpeg `-loglevel warning -hide_banner`
///   (shows warnings and the live encoding status line)
/// - subforge Verbose → ffmpeg defaults (full progress + info)
fn apply_ffmpeg_loglevel(cmd: &mut Command) {
    use crate::logging::{Level, level};
    match level() {
        Level::Quiet => {
            cmd.args(["-loglevel", "error", "-hide_banner", "-nostats"]);
        }
        Level::Normal => {
            cmd.args(["-loglevel", "warning", "-hide_banner"]);
        }
        Level::Verbose => {
            // Leave defaults so power users see what's happening.
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Time spec validation
// ─────────────────────────────────────────────────────────────────────────────

/// Validate an ffmpeg time spec (`--ss` / `--duration`).
///
/// ffmpeg accepts:
/// - bare seconds: `60`, `60.5`, `0.001`
/// - HH:MM:SS[.mmm]: `00:01:30`, `1:30:00.500`
/// - MM:SS[.mmm]: `01:30`
///
/// We pre-validate so a typo like `--ss abc` produces a clear subforge-side
/// error instead of ffmpeg's cryptic "Error opening input files: Invalid
/// argument" pointing at an unrelated input file.
pub fn validate_time_spec(name: &str, value: &str) -> Result<(), String> {
    let s = value.trim();
    if s.is_empty() {
        return Err(format!("{name} cannot be empty"));
    }
    // Bare number?
    if let Ok(n) = s.parse::<f64>() {
        if n < 0.0 {
            return Err(format!("{name} must be ≥ 0 (got '{value}')"));
        }
        return Ok(());
    }
    // HH:MM:SS[.mmm] or MM:SS[.mmm]
    let parts: Vec<&str> = s.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return Err(format!(
            "{name} '{value}' is not a valid time (use seconds, MM:SS, or HH:MM:SS[.mmm])"
        ));
    }
    for (i, p) in parts.iter().enumerate() {
        let parsed: Result<f64, _> = p.parse();
        let n = parsed.map_err(|_| {
            format!(
                "{name} '{value}': segment {} ('{}') is not numeric",
                i + 1,
                p
            )
        })?;
        if n < 0.0 {
            return Err(format!("{name} '{value}' has negative segment '{p}'"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse() {
        assert_eq!(Mode::parse("hard").unwrap(), Mode::Hard);
        assert_eq!(Mode::parse("SOFT").unwrap(), Mode::Soft);
        assert_eq!(Mode::parse("both").unwrap(), Mode::Both);
        assert!(Mode::parse("garbage").is_err());
    }

    #[test]
    fn position_alignment() {
        assert_eq!(Position::Bottom.alignment(), 2);
        assert_eq!(Position::Center.alignment(), 5);
        assert_eq!(Position::Top.alignment(), 8);
        assert_eq!(Position::BottomLeft.alignment(), 1);
        assert_eq!(Position::BottomRight.alignment(), 3);
        assert_eq!(Position::TopLeft.alignment(), 7);
        assert_eq!(Position::TopRight.alignment(), 9);
        assert_eq!(Position::MiddleLeft.alignment(), 4);
        assert_eq!(Position::MiddleRight.alignment(), 6);
    }

    #[test]
    fn position_parse_aliases() {
        assert_eq!(Position::parse("bottom").unwrap(), Position::Bottom);
        assert_eq!(Position::parse("bottom-center").unwrap(), Position::Bottom);
        assert_eq!(
            Position::parse("BOTTOM-LEFT").unwrap(),
            Position::BottomLeft
        );
        assert_eq!(Position::parse("top-right").unwrap(), Position::TopRight);
        assert_eq!(Position::parse("middle").unwrap(), Position::Center);
        assert_eq!(Position::parse("right").unwrap(), Position::MiddleRight);
        assert!(Position::parse("upside-down").is_err());
    }

    #[test]
    fn sanitize_style_value_strips_commas() {
        // "Arial, Black" would split force_style at the comma, dropping "Black".
        // We replace with space, keeping the spirit of the value.
        assert_eq!(sanitize_style_value("Arial, Black"), "Arial  Black");
    }

    #[test]
    fn sanitize_style_value_strips_apostrophes() {
        assert_eq!(sanitize_style_value("It's a font"), "Its a font");
    }

    #[test]
    fn build_force_style_handles_comma_in_font_name() {
        let o = Options {
            font: Some("Arial, Black".into()),
            font_size: Some(24),
            ..Default::default()
        };
        let s = build_force_style(&o);
        // Must NOT contain raw "Arial, Black" — that would corrupt parsing.
        assert!(!s.contains("Arial, Black"));
        assert!(s.contains("FontName=Arial"));
        assert!(s.contains("FontSize=24"));
    }

    #[test]
    fn build_force_style_width_ratio_90_gives_5pct_margins() {
        // PlayResX=384 for SRT; (100-90)*384/200 = 19.2 → 19 each side.
        // 2*19/384 ≈ 9.9% total margin, leaving ~90% for text.
        let o = Options {
            width_ratio_percent: Some(90),
            ..Default::default()
        };
        let s = build_force_style(&o);
        assert!(s.contains("MarginL=19"));
        assert!(s.contains("MarginR=19"));
    }

    #[test]
    fn build_force_style_width_ratio_100_gives_zero_margin() {
        let o = Options {
            width_ratio_percent: Some(100),
            ..Default::default()
        };
        let s = build_force_style(&o);
        assert!(s.contains("MarginL=0"));
        assert!(s.contains("MarginR=0"));
    }

    #[test]
    fn build_force_style_width_ratio_0_emits_nothing() {
        let o = Options {
            width_ratio_percent: Some(0),
            ..Default::default()
        };
        let s = build_force_style(&o);
        assert!(!s.contains("MarginL"));
        assert!(!s.contains("MarginR"));
    }

    // ---- SRT → ASS conversion ----

    #[test]
    fn ass_time_format() {
        assert_eq!(ass_time(0), "0:00:00.00");
        assert_eq!(ass_time(1500), "0:00:01.50");
        assert_eq!(ass_time(3_661_500), "1:01:01.50");
    }

    #[test]
    fn rrggbb_to_ass_bgr_yellow() {
        // FFFF00 (RGB) → BGR: 00FFFF
        assert_eq!(rrggbb_to_ass_bgr("FFFF00").unwrap(), "&H0000FFFF");
    }

    #[test]
    fn html_font_to_ass_yellow() {
        let s = html_font_to_ass("<font color=\"#FFFF00\">你好</font>");
        assert!(s.contains(r"{\c&H0000FFFF&}"));
        assert!(s.contains("你好"));
        assert!(s.ends_with(r"{\r}"));
    }

    #[test]
    fn html_font_to_ass_newlines_become_hard_breaks() {
        let s = html_font_to_ass("line1\nline2");
        assert_eq!(s, "line1\\Nline2");
    }

    #[test]
    fn srt_to_ass_includes_wrapstyle_1() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nhello\n";
        let opts = Options {
            width_ratio_percent: Some(90),
            ..Default::default()
        };
        let ass = srt_to_ass(srt, 1280, 720, &opts);
        assert!(ass.contains("WrapStyle: 1"));
        assert!(ass.contains("PlayResX: 1280"));
        assert!(ass.contains("PlayResY: 720"));
        // 90% of 1280 → 64px margin each side
        assert!(ass.contains("MarginL=") || ass.contains(",64,64,"));
    }

    #[test]
    fn srt_to_ass_emits_dialogue_per_cue() {
        let srt =
            "1\n00:00:01,000 --> 00:00:03,000\nhello\n\n2\n00:00:04,000 --> 00:00:06,000\nworld\n";
        let ass = srt_to_ass(srt, 1280, 720, &Options::default());
        let dialogues: Vec<&str> = ass.lines().filter(|l| l.starts_with("Dialogue:")).collect();
        assert_eq!(dialogues.len(), 2);
        assert!(dialogues[0].contains("0:00:01.00"));
        assert!(dialogues[0].contains("0:00:03.00"));
        assert!(dialogues[0].contains("hello"));
    }

    #[test]
    fn encoder_aliases() {
        assert_eq!(Encoder::parse("x264").unwrap(), Encoder::X264);
        assert_eq!(Encoder::parse("h264").unwrap(), Encoder::X264);
        assert_eq!(Encoder::parse("nvenc").unwrap(), Encoder::NvencH264);
        assert_eq!(Encoder::parse("nvenc-hevc").unwrap(), Encoder::NvencHevc);
        assert!(Encoder::parse("unknown").is_err());
    }

    #[test]
    fn rrggbb_to_ass_white() {
        assert_eq!(rrggbb_to_ass("FFFFFF"), Some("&H00FFFFFF&".into()));
    }

    #[test]
    fn rrggbb_to_ass_red_swaps_channels() {
        // RR=FF, GG=00, BB=00 → libass BGR = 0000FF (B=00, G=00, R=FF)
        // We expect &H00 + BB GG RR (hex pairs)
        assert_eq!(rrggbb_to_ass("FF0000"), Some("&H000000FF&".into()));
    }

    #[test]
    fn rrggbb_to_ass_blue_swaps_channels() {
        // RR=00, GG=00, BB=FF → BGR = FF0000
        assert_eq!(rrggbb_to_ass("0000FF"), Some("&H00FF0000&".into()));
    }

    #[test]
    fn rrggbb_strips_hash_prefix() {
        assert_eq!(rrggbb_to_ass("#FFFFFF"), Some("&H00FFFFFF&".into()));
    }

    #[test]
    fn rrggbb_rejects_malformed() {
        assert_eq!(rrggbb_to_ass("FFFF"), None);
        assert_eq!(rrggbb_to_ass("GGGGGG"), None);
        assert_eq!(rrggbb_to_ass(""), None);
    }

    #[test]
    fn build_force_style_empty_when_no_options() {
        let o = Options::default();
        assert_eq!(build_force_style(&o), "");
    }

    #[test]
    fn build_force_style_combines_fields() {
        let o = Options {
            font: Some("Source Han Sans".into()),
            font_size: Some(22),
            font_color: Some("FFFFFF".into()),
            outline_color: Some("000000".into()),
            outline_width: Some(2),
            position: Some(Position::Bottom),
            margin_v: Some(40),
            ..Default::default()
        };
        let s = build_force_style(&o);
        assert!(s.contains("FontName=Source Han Sans"));
        assert!(s.contains("FontSize=22"));
        assert!(s.contains("PrimaryColour=&H00FFFFFF&"));
        assert!(s.contains("OutlineColour=&H00000000&"));
        assert!(s.contains("Outline=2"));
        assert!(s.contains("Alignment=2"));
        assert!(s.contains("MarginV=40"));
    }

    #[test]
    fn build_force_style_raw_overrides() {
        let o = Options {
            font: Some("Arial".into()),
            style_raw: Some("FontName=Override,FontSize=99".into()),
            ..Default::default()
        };
        // Raw bypasses the wrapper completely.
        assert_eq!(build_force_style(&o), "FontName=Override,FontSize=99");
    }

    #[test]
    fn validate_style_raw_accepts_clean_input() {
        assert!(validate_style_raw(None).is_ok());
        assert!(validate_style_raw(Some("")).is_ok());
        assert!(validate_style_raw(Some("FontName=Arial,Bold=1")).is_ok());
        // Filter-graph metacharacters are fine inside the single-quoted region.
        assert!(validate_style_raw(Some("FontName=Arial:Bold=1")).is_ok());
    }

    #[test]
    fn validate_style_raw_rejects_apostrophe() {
        let err = validate_style_raw(Some("FontName=It's a font")).unwrap_err();
        assert!(err.contains("非法字符"), "{err}");
        assert!(err.contains('\''), "{err}");
    }

    #[test]
    fn validate_style_raw_rejects_newline() {
        let err = validate_style_raw(Some("FontName=foo\n--inject=evil")).unwrap_err();
        assert!(err.contains("非法字符"), "{err}");
    }
    #[test]
    fn build_force_style_skips_invalid_color() {
        let o = Options {
            font_color: Some("not-a-color".into()),
            ..Default::default()
        };
        // Invalid color → field omitted, no garbage in output.
        assert_eq!(build_force_style(&o), "");
    }

    #[test]
    fn map_nvenc_preset_translation() {
        assert_eq!(map_nvenc_preset("veryfast"), "p1");
        assert_eq!(map_nvenc_preset("fast"), "p3");
        assert_eq!(map_nvenc_preset("medium"), "p4");
        assert_eq!(map_nvenc_preset("slow"), "p6");
        assert_eq!(map_nvenc_preset("veryslow"), "p7");
        assert_eq!(map_nvenc_preset("garbage"), "p4"); // safe default
    }

    #[test]
    fn double_bitrate_handles_units() {
        assert_eq!(double_bitrate("5M").unwrap(), "10M");
        assert_eq!(double_bitrate("8000k").unwrap(), "16000k");
        assert_eq!(double_bitrate("123").unwrap(), "246");
        assert_eq!(double_bitrate("2.5M").unwrap(), "5M");
        assert!(double_bitrate("not-a-rate").is_none());
    }

    #[test]
    fn ffmpeg_lang_tag_known() {
        assert_eq!(ffmpeg_lang_tag("zh-Hans"), "zho");
        assert_eq!(ffmpeg_lang_tag("ja"), "jpn");
        assert_eq!(ffmpeg_lang_tag("en"), "eng");
        // Pass through unknown
        assert_eq!(ffmpeg_lang_tag("xx-custom"), "xx-custom");
    }

    #[test]
    fn escape_path_simple() {
        assert_eq!(
            escape_subtitles_path(Path::new("/tmp/sub.srt")),
            "'/tmp/sub.srt'"
        );
    }

    #[test]
    fn escape_path_colon_and_brackets() {
        let p = Path::new("/dir:[x];/y.srt");
        assert_eq!(escape_subtitles_path(p), "'/dir\\:\\[x\\]\\;/y.srt'");
    }

    #[test]
    fn derive_output_uses_marker() {
        let p = Path::new("/tmp/video.mp4");
        let out = derive_output_path(p, None, "_captioned", None);
        assert_eq!(out, PathBuf::from("/tmp/video_captioned.mp4"));
    }

    #[test]
    fn derive_output_respects_override() {
        let p = Path::new("/tmp/video.mp4");
        let out = derive_output_path(p, Some("/elsewhere/x.mp4"), "_captioned", None);
        assert_eq!(out, PathBuf::from("/elsewhere/x.mp4"));
    }

    #[test]
    fn derive_output_can_force_extension() {
        let p = Path::new("/tmp/video.mp4");
        let out = derive_output_path(p, None, "_softsub", Some("mkv"));
        assert_eq!(out, PathBuf::from("/tmp/video_softsub.mkv"));
    }

    #[test]
    fn insert_marker_before_ext_basic() {
        let p = Path::new("/elsewhere/x.mp4");
        assert_eq!(
            insert_marker_before_ext(p, "_softsub"),
            PathBuf::from("/elsewhere/x_softsub.mp4")
        );
    }

    #[test]
    fn insert_marker_before_ext_no_extension() {
        let p = Path::new("/elsewhere/x");
        assert_eq!(
            insert_marker_before_ext(p, "_softsub"),
            PathBuf::from("/elsewhere/x_softsub")
        );
    }

    #[test]
    fn insert_marker_before_ext_handles_multi_dot() {
        // file.tar.gz behavior: extension is just "gz", stem is "file.tar"
        let p = Path::new("/tmp/file.tar.gz");
        assert_eq!(
            insert_marker_before_ext(p, "_x"),
            PathBuf::from("/tmp/file.tar_x.gz")
        );
    }

    #[test]
    fn validate_time_spec_accepts_seconds() {
        assert!(validate_time_spec("--ss", "60").is_ok());
        assert!(validate_time_spec("--ss", "60.5").is_ok());
        assert!(validate_time_spec("--ss", "0").is_ok());
    }

    #[test]
    fn validate_time_spec_accepts_hms() {
        assert!(validate_time_spec("--ss", "00:01:30").is_ok());
        assert!(validate_time_spec("--ss", "1:30:00.500").is_ok());
        assert!(validate_time_spec("--ss", "01:30").is_ok());
    }

    #[test]
    fn validate_time_spec_rejects_garbage() {
        assert!(validate_time_spec("--ss", "abc").is_err());
        assert!(validate_time_spec("--ss", "").is_err());
        assert!(validate_time_spec("--ss", "1:2:3:4").is_err());
        assert!(validate_time_spec("--ss", "1:abc:3").is_err());
        assert!(validate_time_spec("--ss", "-5").is_err());
    }
}
