use clap::Parser;
use subforge::cli::{Cli, Commands};
use subforge::logging::{self, Level};
use subforge::{commands, config, gpu, subtitle, synthesize, transcribe};

#[tokio::main]
async fn main() {
    // Verbosity setup: env var first, then CLI flags override.
    logging::init_from_env();
    let cli = Cli::parse();
    if cli.quiet {
        logging::set_level(Level::Quiet);
    } else if cli.verbose {
        logging::set_level(Level::Verbose);
    }

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(config::resolve_config_path);
    let cfg = config::Config::load(&config_path);

    let result = run(cli, &config_path, cfg).await;
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// All command dispatch lives here so a single `?`-returning entry can carry
/// every error (clap parse, option validation, pipeline runtime) through one
/// uniform handler at the bottom of `main`.
async fn run(cli: Cli, config_path: &std::path::Path, cfg: config::Config) -> Result<(), String> {
    match cli.command {
        Commands::Transcribe {
            input,
            output,
            asr,
            format,
        } => {
            let mut cfg = cfg;
            if let Some(a) = asr {
                cfg.asr = a;
            }
            gpu::prepare_for_faster_whisper(&mut cfg, config_path).await?;
            let p = transcribe::run(&input, output.as_deref(), &cfg.asr, format.as_deref(), &cfg)
                .await?;
            println!("{}", p.display());
            Ok(())
        }
        Commands::Subtitle {
            input,
            output,
            translator,
            target_language,
            layout,
            no_translate,
            thread_num,
            batch_size,
        } => {
            let mut cfg = cfg;
            apply_overrides(&mut cfg, translator, target_language, None);
            if let Some(l) = layout {
                cfg.layout = l;
            }
            if let Some(n) = thread_num {
                cfg.thread_num = n;
            }
            if let Some(b) = batch_size {
                cfg.batch_size = b;
            }
            if no_translate {
                cfg.translator = String::new();
            }
            let p = subtitle::run(&input, output.as_deref(), &cfg, None).await?;
            println!("{}", p.display());
            Ok(())
        }
        Commands::Synthesize {
            input,
            subtitle,
            output,
            mode,
            font,
            font_size,
            font_color,
            outline_color,
            outline_width,
            position,
            margin_v,
            style,
            encoder,
            crf,
            preset,
            max_bitrate,
            width_ratio,
            ss,
            duration,
        } => {
            let mut cfg = cfg;
            let opts = build_synthesize_options(
                mode,
                font,
                font_size,
                font_color,
                outline_color,
                outline_width,
                position,
                margin_v,
                style,
                encoder,
                crf,
                preset,
                max_bitrate,
                width_ratio,
                ss,
                duration,
                &cfg,
            )?;
            if uses_nvenc(&opts) {
                gpu::prepare_for_cuda_task(&mut cfg, config_path, "NVENC GPU").await?;
            }
            let p = synthesize::run(&input, &subtitle, output.as_deref(), &opts, &cfg).await?;
            println!("{}", p.display());
            Ok(())
        }
        Commands::Translate {
            input,
            output,
            asr,
            translator,
            target_language,
            no_cache,
            keep_intermediate,
        } => {
            let mut cfg = cfg;
            apply_overrides(&mut cfg, translator, target_language, asr);
            gpu::prepare_for_faster_whisper(&mut cfg, config_path).await?;
            let opts = commands::process::Options {
                no_synthesize: true,
                no_cache,
                keep_intermediate,
                synth: synthesize::Options::default(),
            };
            let p = commands::process::run(&input, output.as_deref(), opts, &cfg).await?;
            println!("{}", p.display());
            Ok(())
        }
        Commands::Process {
            input,
            output,
            asr,
            translator,
            target_language,
            no_synthesize,
            no_cache,
            keep_intermediate,
            synth_mode,
            font,
            font_size,
            font_color,
            outline_color,
            outline_width,
            position,
            margin_v,
            style,
            encoder,
            crf,
            preset,
            max_bitrate,
            width_ratio,
        } => {
            let mut cfg = cfg;
            apply_overrides(&mut cfg, translator, target_language, asr);
            gpu::prepare_for_faster_whisper(&mut cfg, config_path).await?;
            let synth = build_synthesize_options(
                synth_mode,
                font,
                font_size,
                font_color,
                outline_color,
                outline_width,
                position,
                margin_v,
                style,
                encoder,
                crf,
                preset,
                max_bitrate,
                width_ratio,
                None,
                None, // process doesn't expose --ss / --duration (would crop the whole pipeline)
                &cfg,
            )?;
            let opts = commands::process::Options {
                no_synthesize,
                no_cache,
                keep_intermediate,
                synth,
            };
            if !no_synthesize && uses_nvenc(&opts.synth) {
                gpu::prepare_for_cuda_task(&mut cfg, config_path, "NVENC GPU").await?;
            }
            let p = commands::process::run(&input, output.as_deref(), opts, &cfg).await?;
            println!("{}", p.display());
            Ok(())
        }
        Commands::Config { command } => commands::config_cmd::handle(command, config_path, &cfg),
        Commands::Model { command } => commands::model::handle(command, &cfg).await,
        Commands::Gpu { set } => gpu::handle_command(config_path, &cfg, set).await,
        Commands::Eval {
            hypothesis,
            reference,
            language,
        } => commands::eval::handle(&hypothesis, &reference, language.as_deref(), &cfg).await,
        Commands::Setup { compute, force } => commands::setup::handle(&compute, force, &cfg).await,
        Commands::Doctor => commands::doctor::handle(&cfg).await,
        Commands::Cache { command } => commands::cache_cmd::handle(command, &cfg),
    }
}

fn apply_overrides(
    cfg: &mut config::Config,
    translator: Option<String>,
    target_language: Option<String>,
    asr: Option<String>,
) {
    if let Some(t) = translator {
        cfg.translator = t;
    }
    if let Some(l) = target_language {
        cfg.target_language = l;
    }
    if let Some(a) = asr {
        cfg.asr = a;
    }
}

fn uses_nvenc(opts: &synthesize::Options) -> bool {
    matches!(
        opts.encoder,
        Some(synthesize::Encoder::NvencH264 | synthesize::Encoder::NvencHevc)
    )
}

/// Three-layer precedence helpers for synthesis options.
///
/// Order: explicit CLI flag > config.toml value > built-in default (None).
/// "Unset" in config means empty string for `String` fields and 0 for
/// numeric fields.
fn cli_or_cfg_string(cli: Option<String>, cfg: &str) -> Option<String> {
    cli.or_else(|| {
        if cfg.is_empty() {
            None
        } else {
            Some(cfg.to_string())
        }
    })
}

fn cli_or_cfg_u32(cli: Option<u32>, cfg: u32) -> Option<u32> {
    cli.or(if cfg == 0 { None } else { Some(cfg) })
}

fn cli_or_cfg_u8(cli: Option<u8>, cfg: u8) -> Option<u8> {
    cli.or(if cfg == 0 { None } else { Some(cfg) })
}

#[allow(clippy::too_many_arguments)]
fn build_synthesize_options(
    cli_mode: Option<String>,
    cli_font: Option<String>,
    cli_font_size: Option<u32>,
    cli_font_color: Option<String>,
    cli_outline_color: Option<String>,
    cli_outline_width: Option<u32>,
    cli_position: Option<String>,
    cli_margin_v: Option<u32>,
    cli_style_raw: Option<String>,
    cli_encoder: Option<String>,
    cli_crf: Option<u8>,
    cli_preset: Option<String>,
    cli_max_bitrate: Option<String>,
    cli_width_ratio: Option<u8>,
    ss: Option<String>,
    duration: Option<String>,
    cfg: &config::Config,
) -> Result<synthesize::Options, String> {
    // Apply CLI > config > builtin layering for every overridable field.
    let mode_str = cli_or_cfg_string(cli_mode, &cfg.synth_mode).unwrap_or_else(|| "hard".into());
    let mode = synthesize::Mode::parse(&mode_str)?;

    let font = cli_or_cfg_string(cli_font, &cfg.synth_font);
    let font_size = cli_or_cfg_u32(cli_font_size, cfg.synth_font_size);
    let font_color = cli_or_cfg_string(cli_font_color, &cfg.synth_font_color);
    let outline_color = cli_or_cfg_string(cli_outline_color, &cfg.synth_outline_color);
    let outline_width = cli_or_cfg_u32(cli_outline_width, cfg.synth_outline_width);
    let margin_v = cli_or_cfg_u32(cli_margin_v, cfg.synth_margin_v);
    let max_bitrate = cli_or_cfg_string(cli_max_bitrate, &cfg.synth_max_bitrate);
    let preset = cli_or_cfg_string(cli_preset, &cfg.synth_preset);
    let crf = cli_or_cfg_u8(cli_crf, cfg.synth_crf);
    let width_ratio_percent = cli_or_cfg_u8(cli_width_ratio, cfg.synth_width_ratio);

    // Position and Encoder both go through enum parsers; do that AFTER the
    // layering so a bad config value produces a clear error pointing at the
    // config key, not silent fallback.
    let position = match cli_or_cfg_string(cli_position, &cfg.synth_position) {
        Some(s) => Some(synthesize::Position::parse(&s)?),
        None => None,
    };
    let encoder = match cli_or_cfg_string(cli_encoder, &cfg.synth_encoder) {
        Some(s) => Some(synthesize::Encoder::parse(&s)?),
        None => None,
    };

    if let Some(s) = &ss {
        synthesize::validate_time_spec("--ss", s)?;
    }
    if let Some(s) = &duration {
        synthesize::validate_time_spec("--duration", s)?;
    }

    Ok(synthesize::Options {
        mode,
        font,
        font_size,
        font_color,
        outline_color,
        outline_width,
        position,
        margin_v,
        style_raw: cli_style_raw,
        encoder,
        crf,
        preset,
        max_bitrate,
        width_ratio_percent,
        ss,
        duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_overrides_cfg() {
        assert_eq!(
            cli_or_cfg_string(Some("cli".into()), "cfg"),
            Some("cli".into())
        );
        assert_eq!(cli_or_cfg_u32(Some(99), 50), Some(99));
    }

    #[test]
    fn cfg_used_when_cli_absent() {
        assert_eq!(cli_or_cfg_string(None, "cfg"), Some("cfg".into()));
        assert_eq!(cli_or_cfg_u32(None, 50), Some(50));
    }

    #[test]
    fn neither_set_returns_none() {
        assert_eq!(cli_or_cfg_string(None, ""), None);
        assert_eq!(cli_or_cfg_u32(None, 0), None);
        assert_eq!(cli_or_cfg_u8(None, 0), None);
    }
}
