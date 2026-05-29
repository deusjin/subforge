use crate::config::Config;
use crate::progress::format_elapsed;
use crate::translate;
use crate::util::{self, Segment};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Run subtitle translation.
///
/// `context_dir_override`: if provided, TM (glossary/memory) will be stored
/// relative to this directory instead of the input SRT's parent. This is used
/// by `process` to anchor TM to the original video's directory rather than
/// the intermediate output directory.
pub async fn run(
    input: &str,
    output: Option<&str>,
    cfg: &Config,
    context_dir_override: Option<&Path>,
) -> Result<PathBuf, String> {
    let input_path = PathBuf::from(input);
    if !input_path.exists() {
        return Err(format!("file not found: {input}"));
    }

    let content = util::read_subtitle_file(&input_path)?;
    let segments = util::parse_srt(&content);
    if segments.is_empty() {
        return Err("no subtitle segments found".into());
    }

    // Translate if translator is configured
    let translated = if !cfg.translator.is_empty() {
        let texts: Vec<String> = segments.iter().map(|s| s.text.clone()).collect();
        crate::log_info!(
            "  translating {} segments with {}...",
            texts.len(),
            cfg.translator
        );
        let context_dir = context_dir_override.or(input_path.parent());
        let started = Instant::now();
        let translated = translate::translate_batch(&texts, &segments, cfg, context_dir).await?;
        if context_dir_override.is_none() {
            crate::log_info!(
                "  translation completed in {}",
                format_elapsed(started.elapsed())
            );
        }
        translated
    } else {
        vec![String::new(); segments.len()]
    };

    // Render output based on layout + per-line colors
    let target_tag = rrggbb_to_ass_color_tag(&cfg.target_color);
    let source_tag = rrggbb_to_ass_color_tag(&cfg.source_color);
    let output_segments: Vec<Segment> = segments
        .iter()
        .zip(translated.iter())
        .enumerate()
        .map(|(i, (seg, translation))| {
            let text = render_layout(
                &cfg.layout,
                &seg.text,
                translation,
                target_tag.as_deref(),
                source_tag.as_deref(),
            );
            Segment {
                index: i + 1,
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                text,
            }
        })
        .collect();

    let out_path = match output {
        Some(o) => PathBuf::from(o),
        None => {
            let stem = input_path.file_stem().unwrap_or_default().to_string_lossy();
            input_path.with_file_name(util::translated_output_name(&stem, "srt"))
        }
    };
    let content = util::render_srt(&output_segments);
    std::fs::write(&out_path, &content).map_err(|e| e.to_string())?;
    Ok(out_path)
}

/// Compose one cue's text from source + translation, applying the requested
/// layout and per-line color overrides if any.
///
/// We use the HTML-style `<font color="#RRGGBB">...</font>` tag — this is the
/// de-facto SRT extension that libass (used by ffmpeg's `subtitles=` filter)
/// and most modern players (mpv, VLC, MPC-HC) honor. ASS-style override tags
/// like `{\c&Hbbggrr&}` look correct on paper but libass strips them when
/// reading SRT, so they don't actually colorize anything.
fn render_layout(
    layout: &str,
    original: &str,
    translation: &str,
    target_tag: Option<&str>,
    source_tag: Option<&str>,
) -> String {
    let wrap = |text: &str, tag: Option<&str>| -> String {
        match tag {
            Some(color) => format!(r#"<font color="{color}">{text}</font>"#),
            None => text.to_string(),
        }
    };
    let target = wrap(translation, target_tag);
    let source = wrap(original, source_tag);

    match layout {
        "target-above" => {
            if translation.is_empty() {
                source
            } else {
                format!("{target}\n{source}")
            }
        }
        "source-above" => {
            if translation.is_empty() {
                source
            } else {
                format!("{source}\n{target}")
            }
        }
        "target-only" => {
            if translation.is_empty() {
                source
            } else {
                target
            }
        }
        _ => source,
    }
}

/// Convert a configured RRGGBB color to an HTML `<font color>` value
/// (e.g. `#FFFF00`). Returns None for empty / malformed input so the caller
/// emits no tag at all.
fn rrggbb_to_ass_color_tag(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", s.to_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrggbb_yellow_returns_html_color() {
        assert_eq!(rrggbb_to_ass_color_tag("FFFF00").unwrap(), "#FFFF00");
    }

    #[test]
    fn rrggbb_normalizes_case() {
        assert_eq!(rrggbb_to_ass_color_tag("ffff00").unwrap(), "#FFFF00");
    }

    #[test]
    fn rrggbb_strips_hash() {
        assert_eq!(rrggbb_to_ass_color_tag("#FFFFFF").unwrap(), "#FFFFFF");
    }

    #[test]
    fn rrggbb_invalid_returns_none() {
        assert!(rrggbb_to_ass_color_tag("").is_none());
        assert!(rrggbb_to_ass_color_tag("FFF").is_none());
        assert!(rrggbb_to_ass_color_tag("ZZZZZZ").is_none());
    }

    #[test]
    fn render_target_above_no_color() {
        let r = render_layout("target-above", "hello", "你好", None, None);
        assert_eq!(r, "你好\nhello");
    }

    #[test]
    fn render_target_above_with_target_color() {
        let yellow = rrggbb_to_ass_color_tag("FFFF00");
        let r = render_layout("target-above", "hello", "你好", yellow.as_deref(), None);
        assert_eq!(r, "<font color=\"#FFFF00\">你好</font>\nhello");
    }

    #[test]
    fn render_target_above_with_both_colors() {
        let yellow = rrggbb_to_ass_color_tag("FFFF00");
        let white = rrggbb_to_ass_color_tag("FFFFFF");
        let r = render_layout(
            "target-above",
            "hello",
            "你好",
            yellow.as_deref(),
            white.as_deref(),
        );
        assert!(r.contains("<font color=\"#FFFF00\">你好</font>"));
        assert!(r.contains("<font color=\"#FFFFFF\">hello</font>"));
    }

    #[test]
    fn render_target_above_no_translation_falls_back_to_source() {
        let yellow = rrggbb_to_ass_color_tag("FFFF00");
        let r = render_layout("target-above", "hello", "", yellow.as_deref(), None);
        assert_eq!(r, "hello");
    }

    #[test]
    fn render_target_only_drops_source() {
        let yellow = rrggbb_to_ass_color_tag("FFFF00");
        let r = render_layout("target-only", "hello", "你好", yellow.as_deref(), None);
        assert_eq!(r, "<font color=\"#FFFF00\">你好</font>");
    }
}
