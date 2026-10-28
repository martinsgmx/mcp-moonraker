//! Error types used across the crate.
//!
//! Errors are deliberately split between *operational* failures (the Moonraker
//! instance is down, a response was malformed) and *policy* failures (the
//! requested action is disabled or needs approval). Policy failures are safe to
//! show to an agent; operational failures are redacted before leaving the
//! process.

use thiserror::Error;

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error type.
#[derive(Debug, Error)]
pub enum Error {
    /// Invalid or unsafe configuration.
    #[error("configuration error: {0}")]
    Config(String),

    /// The configured Moonraker URL could not be parsed or used.
    #[error("invalid Moonraker URL: {0}")]
    Url(#[from] url::ParseError),

    /// Transport-level failure talking to Moonraker.
    #[error("Moonraker transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// Moonraker returned a JSON-RPC or HTTP API error.
    #[error("Moonraker API error {code}: {message}")]
    Api {
        /// JSON-RPC or HTTP status code.
        code: i64,
        /// Human readable message from Moonraker.
        message: String,
    },

    /// Moonraker returned an unexpected payload.
    #[error("invalid Moonraker response: {0}")]
    InvalidResponse(String),

    /// The request was refused by the local policy engine.
    #[error("policy denied: {0}")]
    Policy(String),

    /// A tool was called with missing, malformed, or unknown arguments.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),

    /// An approval could not be created, found, or verified.
    #[error("approval error: {0}")]
    Approval(String),

    /// Camera snapshot retrieval failed.
    #[error("camera error: {0}")]
    Camera(String),

    /// Filesystem failure (approval store, audit log, state directory).
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// TOML configuration parsing failure.
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),
}

impl Error {
    /// Build an API error from a status code and message.
    pub fn api(code: impl Into<i64>, message: impl Into<String>) -> Self {
        Self::Api {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Return a short, secret-free description suitable for an MCP client.
    #[must_use]
    pub fn public_message(&self) -> String {
        match self {
            Self::Http(err) => {
                // reqwest errors can include the full URL (which is usually not
                // secret) but never include headers. Keep it short.
                format!("Moonraker is unreachable or returned a transport error: {err}")
            }
            Self::Api { code, message } => format!("Moonraker API error {code}: {message}"),
            Self::Config(message)
            | Self::InvalidResponse(message)
            | Self::Policy(message)
            | Self::InvalidArguments(message)
            | Self::Approval(message)
            | Self::Camera(message) => message.clone(),
            Self::Url(err) => format!("invalid Moonraker URL: {err}"),
            Self::Io(err) => format!("filesystem error: {err}"),
            Self::Json(err) => format!("JSON error: {err}"),
            Self::Toml(err) => format!("TOML error: {err}"),
        }
    }
}
