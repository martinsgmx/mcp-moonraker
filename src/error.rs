//! Error types used across the crate.
//!
//! Errors are deliberately split between *operational* failures (the Moonraker
//! instance is down, a response was malformed) and *policy* failures (the
//! requested action is disabled or needs approval). Policy failures are safe to
//! show to an agent; operational failures are redacted before leaving the
//! process.

use std::io::ErrorKind;

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
    ///
    /// Operational failures carry one actionable hint per failure class so an
    /// agent (or a human reading the transcript) can distinguish a wrong URL
    /// from a firewall drop from a rejected API key without opening a shell.
    #[must_use]
    pub fn public_message(&self) -> String {
        match self {
            Self::Http(err) => {
                // reqwest errors can include the full URL (which is usually not
                // secret) but never include headers.
                format!(
                    "Moonraker is unreachable or returned a transport error: {err} ({})",
                    transport_hint(err)
                )
            }
            Self::Api { code, message } if *code == 401 => format!(
                "Moonraker API error 401: {message}. Moonraker rejected the request: \
                 set MOONRAKER_API_KEY to the value from \
                 'curl http://<printer>:7125/access/api_key' (call it from the \
                 printer host), rotate the key if it changed, and check that \
                 moonraker.conf does not enable force_logins, which blocks all \
                 non-interactive clients"
            ),
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

/// Failure classes for transport errors, ordered from most specific outward.
enum TransportCause {
    /// TCP connect succeeded to a port with no HTTP server on it, or the TLS
    /// handshake was rejected.
    Refused,
    /// The hostname could not be resolved.
    Dns,
    /// No route, or the connection attempt died on the way.
    Unreachable,
    /// The response did not come back in time.
    Timeout,
    /// TLS certificate rejected (self-signed, wrong host, expired).
    Tls,
    /// Nothing more specific could be determined.
    Other,
}

impl TransportCause {
    fn hint(self) -> &'static str {
        match self {
            Self::Refused => {
                "hint: nothing is listening at that address: verify Moonraker is running \
                (systemctl status moonraker), that MOONRAKER_URL host/port match moonraker.conf \
                [server] host/port, and that 127.0.0.1 is not used from a container or another host"
            }
            Self::Dns => {
                "hint: the hostname could not be resolved on this machine: use the \
                printer's IP address or fix DNS/mDNS resolution on the host running mcp-moonraker"
            }
            Self::Unreachable => {
                "hint: the host or route is unreachable: check IP routing, VPN, \
                firewall, or the printer's network address"
            }
            Self::Timeout => {
                "hint: Moonraker did not respond in time: it may be overloaded or \
                unreachable; confirm with 'curl -v <MOONRAKER_URL>/server/info' before raising \
                moonraker.timeout_secs. For scripts that intentionally run \
                long (homing, TEMPERATURE_WAIT macros) raise moonraker.script_timeout_secs instead"
            }
            Self::Tls => {
                "hint: the TLS certificate was rejected: use a trusted certificate on the \
                reverse proxy, or set moonraker.tls_verify=false for a self-signed host you control"
            }
            Self::Other => {
                "hint: confirm reachability with 'curl -v <MOONRAKER_URL>/server/info' \
                and re-check MOONRAKER_URL"
            }
        }
    }
}

fn cause_from_chain(io_kind: Option<ErrorKind>, text: &str) -> TransportCause {
    let text = text.to_ascii_lowercase();
    if let Some(kind) = io_kind {
        match kind {
            ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset => return TransportCause::Refused,
            ErrorKind::PermissionDenied => return TransportCause::Unreachable,
            _ => {}
        }
    }
    for (needle, cause) in [
        ("connection refused", TransportCause::Refused),
        ("connection aborted", TransportCause::Refused),
        ("connection reset", TransportCause::Refused),
        ("failed to lookup", TransportCause::Dns),
        ("name or service not known", TransportCause::Dns),
        ("network is unreachable", TransportCause::Unreachable),
        ("no route to host", TransportCause::Unreachable),
        ("network unreachable", TransportCause::Unreachable),
        ("certificate", TransportCause::Tls),
        ("invalid peer certificate", TransportCause::Tls),
        ("tls", TransportCause::Tls),
        ("ssl", TransportCause::Tls),
        ("timed out", TransportCause::Timeout),
    ] {
        if text.contains(needle) {
            return cause;
        }
    }
    TransportCause::Other
}

/// Classify a reqwest transport failure and return the matching hint.
fn transport_hint(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return TransportCause::Timeout.hint().to_string();
    }
    // Walk the cause chain collecting io error kinds and message text; reqwest
    // keeps the HTTP details (never headers) in sources for connect and TLS
    // failures while URL data is already stripped via `without_url()`.
    let mut cause: Option<&dyn std::error::Error> = Some(err);
    let mut io_kind: Option<ErrorKind> = None;
    let mut text = String::new();
    while let Some(current) = cause {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            io_kind = Some(io.kind());
        }
        text.push_str(&current.to_string());
        text.push(' ');
        cause = current.source();
    }
    cause_from_chain(io_kind, &text).hint().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn io_connection_refused_maps_to_refused() {
        assert!(matches!(
            cause_from_chain(Some(ErrorKind::ConnectionRefused), "something"),
            TransportCause::Refused
        ));
    }

    #[test]
    fn connection_reset_in_text_maps_to_refused() {
        assert!(matches!(
            cause_from_chain(None, "hyper: connection reset by peer"),
            TransportCause::Refused
        ));
    }

    #[test]
    fn getaddrinfo_failure_maps_to_dns() {
        assert!(matches!(
            cause_from_chain(
                Some(ErrorKind::Other),
                "dns error: failed to lookup address information: Name or service not known"
            ),
            TransportCause::Dns
        ));
    }

    #[test]
    fn routing_failures_map_to_unreachable() {
        assert!(matches!(
            cause_from_chain(None, "network is unreachable"),
            TransportCause::Unreachable
        ));
        assert!(matches!(
            cause_from_chain(None, "no route to host"),
            TransportCause::Unreachable
        ));
        assert!(matches!(
            cause_from_chain(Some(ErrorKind::PermissionDenied), ""),
            TransportCause::Unreachable
        ));
    }

    #[test]
    fn certificate_failures_map_to_tls() {
        assert!(matches!(
            cause_from_chain(None, "invalid peer certificate: UnknownIssuer"),
            TransportCause::Tls
        ));
        assert!(matches!(
            cause_from_chain(None, "rustls: received fatal alert: HandshakeFailure"),
            TransportCause::Tls
        ));
    }

    #[test]
    fn unknown_failures_map_to_other() {
        assert!(matches!(
            cause_from_chain(None, "no idea what happened"),
            TransportCause::Other
        ));
    }

    #[test]
    fn hints_are_single_line_and_secret_free() {
        for cause in [
            TransportCause::Refused,
            TransportCause::Dns,
            TransportCause::Unreachable,
            TransportCause::Timeout,
            TransportCause::Tls,
            TransportCause::Other,
        ] {
            let hint = cause.hint();
            assert!(hint.contains("hint:"));
            assert!(!hint.contains('\n'));
            // never leak a password-style secret placeholder
            assert!(!hint.to_ascii_lowercase().contains("password"));
        }
    }

    #[test]
    fn unauthorized_api_error_carries_key_hint() {
        let err = Error::api(401, "Unauthorized");
        let message = err.public_message();
        assert!(message.contains("MOONRAKER_API_KEY"));
        assert!(message.contains("force_logins"));
    }

    #[test]
    fn other_api_errors_keep_original_shape() {
        assert_eq!(
            Error::api(-32601, "Method not found").public_message(),
            "Moonraker API error -32601: Method not found"
        );
    }
}
