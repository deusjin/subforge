//! Minimal verbosity-aware logging.
//!
//! Replaces the previous ad-hoc `eprintln!` everywhere with a tiny façade
//! that respects a process-global verbosity level. We don't pull in
//! `tracing` or `log` because the surface is small and CLI-only — overkill
//! infrastructure costs more than it saves at this scale.
//!
//! Levels:
//! - `Quiet`   : only errors.
//! - `Normal`  : pipeline progress (default).
//! - `Verbose` : per-batch / per-cue diagnostics.
//!
//! The level can be set programmatically via [`set_level`] or via the
//! `SUBFORGE_LOG` env var (`quiet`/`normal`/`verbose`). The CLI flag
//! `-v / --verbose` and `-q / --quiet` map onto this.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Quiet = 0,
    Normal = 1,
    Verbose = 2,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Normal as u8);

pub fn init_from_env() {
    if let Ok(s) = std::env::var("SUBFORGE_LOG") {
        match s.to_ascii_lowercase().as_str() {
            "quiet" | "0" => set_level(Level::Quiet),
            "verbose" | "debug" | "2" => set_level(Level::Verbose),
            _ => set_level(Level::Normal),
        }
    }
}

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Quiet,
        2 => Level::Verbose,
        _ => Level::Normal,
    }
}

/// Errors are always emitted, regardless of level.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {{
        eprintln!("error: {}", format_args!($($arg)*));
    }};
}

/// Warnings are emitted unless quiet.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {{
        if $crate::logging::level() != $crate::logging::Level::Quiet {
            eprintln!("warning: {}", format_args!($($arg)*));
        }
    }};
}

/// Pipeline progress (single-line) — suppressed under quiet.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {{
        if $crate::logging::level() != $crate::logging::Level::Quiet {
            eprintln!("{}", format_args!($($arg)*));
        }
    }};
}

/// Per-batch diagnostics — only shown when verbose.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {{
        if $crate::logging::level() == $crate::logging::Level::Verbose {
            eprintln!("  [debug] {}", format_args!($($arg)*));
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_round_trip() {
        set_level(Level::Verbose);
        assert_eq!(level(), Level::Verbose);
        set_level(Level::Normal);
        assert_eq!(level(), Level::Normal);
    }
}
