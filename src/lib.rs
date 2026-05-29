//! Library facade for subforge.
//!
//! The CLI binary in `src/main.rs` is a thin wrapper around these modules.
//! Exposing them as a library means:
//!
//! - Integration tests can `use subforge::translate::...` without spawning the
//!   binary.
//! - Future tooling (GUI, server, language bindings) can depend on the same
//!   pipeline code instead of shelling out.
//! - Benchmarks under `benches/` can exercise individual stages.
//!
//! The public API surface is still narrow on purpose: most types are public
//! within their module but only a handful are re-exported here. Treat the
//! module-level visibility as the intended boundary; the re-exports are for
//! convenience only.

pub mod cache;
pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod gpu;
pub mod logging;
pub mod polish;
pub mod progress;
pub mod subtitle;
pub mod synthesize;
pub mod transcribe;
pub mod translate;
pub mod util;

pub use config::Config;
pub use error::{Error, Result};
