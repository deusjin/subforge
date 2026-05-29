//! Crate-wide error type.
//!
//! # Why this exists
//!
//! Most legacy call sites in subforge return `Result<_, String>`, which loses
//! both error chains and retry classification at the boundary. We can't port
//! every signature in one go, but a single `Error` enum with explicit `From`
//! impls lets us:
//!
//! - Preserve the underlying source error (so `eprintln!("{:#}", e)` shows the
//!   chain instead of a flattened string).
//! - Preserve the `Transient`/`Permanent` classification produced by the
//!   network layer for higher-level recovery decisions.
//! - Keep `Result<_, String>` boundaries compiling via the `From<Error> for
//!   String` impl below — adoption can be incremental.
//!
//! New code should prefer `subforge::Result<T>`. Old code stays on
//! `Result<T, String>` until touched.

use std::fmt;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors the subforge library can produce.
#[derive(Debug)]
pub enum Error {
    /// I/O failure (file read/write, ffmpeg/python spawn, etc.).
    Io(std::io::Error),
    /// Network-layer or HTTP failure with retry classification preserved.
    Network { message: String, transient: bool },
    /// Failed to parse data we received (JSON, SRT, response body).
    Parse(String),
    /// User-facing configuration problem (missing api_key, invalid value).
    Config(String),
    /// External tool (ffmpeg, python, suber) returned non-zero or wasn't found.
    Tool(String),
    /// A condition the caller is expected to handle (e.g. cache miss).
    /// Distinct from a programmer bug.
    NotFound(String),
    /// Catch-all string for legacy boundaries we haven't ported yet.
    Other(String),
}

impl Error {
    /// True for transient errors (timeouts, 5xx, 429) that retry could fix.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Error::Network {
                transient: true,
                ..
            }
        )
    }

    /// Build a transient network error.
    pub fn transient(msg: impl Into<String>) -> Self {
        Error::Network {
            message: msg.into(),
            transient: true,
        }
    }

    /// Build a permanent network error (4xx that won't change on retry).
    pub fn permanent(msg: impl Into<String>) -> Self {
        Error::Network {
            message: msg.into(),
            transient: false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Network { message, transient } => {
                let kind = if *transient {
                    "network (transient)"
                } else {
                    "network"
                };
                write!(f, "{kind}: {message}")
            }
            Error::Parse(m) => write!(f, "parse: {m}"),
            Error::Config(m) => write!(f, "config: {m}"),
            Error::Tool(m) => write!(f, "tool: {m}"),
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(e.to_string())
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Other(s.to_string())
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Other(s)
    }
}

/// Boundary into legacy `Result<_, String>` callers. Loses the structured
/// classification but preserves the human-readable form.
impl From<Error> for String {
    fn from(e: Error) -> Self {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_classification_round_trips() {
        let e = Error::transient("timeout");
        assert!(e.is_transient());
        let s: String = e.into();
        assert!(s.contains("timeout"));
    }

    #[test]
    fn permanent_is_not_transient() {
        let e = Error::permanent("http 401");
        assert!(!e.is_transient());
    }

    #[test]
    fn io_preserves_source() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let e: Error = io.into();
        assert!(std::error::Error::source(&e).is_some());
    }
}
