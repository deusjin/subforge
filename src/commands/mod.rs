pub mod cache_cmd;
pub mod config_cmd;
pub mod doctor;
pub mod eval;
pub mod model;
pub mod process;
pub mod setup;

// Re-export CLI types so command handlers can reference them.
pub use crate::cli::{CacheCommand, ConfigCommand, ModelCommand};
