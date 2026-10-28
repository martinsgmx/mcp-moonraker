//! Secret redaction utilities.
//!
//! Rule: every value that leaves the process (tool output, logs, doctor output,
//! approvals, audit records) passes through [`redact_value`] or an explicit
//! allow-list. Redaction is key-based and recursive; it is intentionally
//! conservative because a false positive is much cheaper than a leaked key.

use serde_json::{Map, Value};

/// Key fragments that cause a value to be replaced with `"[REDACTED]"`.
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "password",
    "passwd",
    "secret",
    "token",
    "authorization",
    "private_key",
    "private-key",
    "credential",
    "cookie",
    "session",
    "sudo_password",
];

/// Keys that are safe to expose even when they contain a sensitive fragment.
const SAFE_KEYS: &[&str] = &[
    "token_unique_id",
    "password_set",
    "has_password",
    "has_api_key",
    "token_count",
];

/// Redact a JSON value recursively.
///
/// * Objects: values whose key looks sensitive are replaced by `"[REDACTED]"`.
/// * Arrays: every element is redacted.
/// * Strings: URLs containing `user:pass@` are sanitized.
#[must_use]
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (key, val) in map {
                if is_sensitive_key(key) {
                    out.insert(key.clone(), Value::String("[REDACTED]".to_string()));
                } else {
                    out.insert(key.clone(), redact_value(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::String(text) => Value::String(redact_url_userinfo(text)),
        other => other.clone(),
    }
}

/// Return `true` when a key name is considered sensitive.
#[must_use]
pub fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    if SAFE_KEYS.iter().any(|safe| normalized == *safe) {
        return false;
    }
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
}

/// Remove `user:password@` from URLs while leaving the rest intact.
#[must_use]
pub fn redact_url_userinfo(text: &str) -> String {
    // Fast path: most strings are not URLs and contain no `@`.
    if !text.contains("://") || !text.contains('@') {
        return text.to_string();
    }
    match url::Url::parse(text) {
        Ok(mut url) if url.username() != "" || url.password().is_some() => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.to_string()
        }
        _ => text.to_string(),
    }
}

/// Truncate long text for audit logs without splitting a UTF-8 codepoint.
#[must_use]
pub fn truncate_for_audit(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…[truncated]")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_nested_secrets() {
        let input = json!({
            "authorization": {"enable_api_key": true},
            "mqtt": {"password": "hunter2", "username": "klipper"},
            "server": {"port": 7125},
            "items": [{"api_key": "abc"}],
        });
        let output = redact_value(&input);
        assert_eq!(output["mqtt"]["password"], "[REDACTED]");
        assert_eq!(output["items"][0]["api_key"], "[REDACTED]");
        assert_eq!(output["server"]["port"], 7125);
        // Conservative by design: a sensitive container is redacted wholesale
        // rather than trying to guess which of its children are safe.
        assert_eq!(output["authorization"], "[REDACTED]");
    }

    #[test]
    fn does_not_redact_safe_keys() {
        assert!(!is_sensitive_key("token_unique_id"));
        assert!(!is_sensitive_key("password_set"));
        assert!(is_sensitive_key("api_key"));
        assert!(is_sensitive_key("X-Api-Key"));
    }

    #[test]
    fn strips_url_credentials() {
        assert_eq!(
            redact_url_userinfo("http://user:pass@printer.local:7125/server/info"),
            "http://printer.local:7125/server/info"
        );
    }

    #[test]
    fn truncation_is_utf8_safe() {
        let text = "🦀".repeat(10);
        let out = truncate_for_audit(&text, 3);
        assert!(out.starts_with("🦀🦀🦀"));
        assert!(out.ends_with("[truncated]"));
    }
}
