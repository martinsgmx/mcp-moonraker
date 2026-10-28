//! Argument parsing helpers for MCP tools.
//!
//! Tool schemas are written as JSON Schema, but decoding is done into this small
//! typed helper rather than `serde_json::from_value`. That gives two useful
//! properties:
//!
//! * unknown properties are rejected (a typo in an agent call fails loudly
//!   instead of being silently ignored);
//! * every error message names the offending field and the accepted range.
//!
//! All tools keep their argument structs small and validate against live
//! Moonraker state (heater names, file existence, homed axes) before acting.

use serde_json::{Map, Value};

use crate::error::{Error, Result};

/// A validated tool argument object.
#[derive(Debug, Clone)]
pub struct Args {
    map: Map<String, Value>,
}

impl Args {
    /// Wrap a JSON value, requiring an object and rejecting unknown keys.
    pub fn parse(value: Option<Value>, allowed: &[&str]) -> Result<Self> {
        let value = value.unwrap_or_else(|| Value::Object(Map::new()));
        let map = match value {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            _ => {
                return Err(Error::InvalidArguments(
                    "arguments must be a JSON object".to_string(),
                ));
            }
        };

        if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
            let mut accepted = allowed.to_vec();
            accepted.sort_unstable();
            return Err(Error::InvalidArguments(format!(
                "unknown argument '{key}'; accepted arguments: {}",
                if accepted.is_empty() {
                    "none".to_string()
                } else {
                    accepted.join(", ")
                }
            )));
        }

        Ok(Self { map })
    }

    /// The canonical JSON value, used for approval fingerprints and audit.
    #[must_use]
    pub fn into_value(self) -> Value {
        Value::Object(self.map)
    }

    /// Borrow the underlying value.
    #[must_use]
    pub fn as_value(&self) -> Value {
        Value::Object(self.map.clone())
    }

    /// Whether the field was supplied.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// Optional string, trimmed. Empty strings are rejected.
    pub fn optional_string(&self, key: &str) -> Result<Option<String>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(Error::InvalidArguments(format!(
                        "'{key}' must not be empty"
                    )));
                }
                if trimmed.contains('\0') {
                    return Err(Error::InvalidArguments(format!(
                        "'{key}' must not contain NUL bytes"
                    )));
                }
                Ok(Some(trimmed.to_string()))
            }
            Some(_) => Err(Error::InvalidArguments(format!("'{key}' must be a string"))),
        }
    }

    /// Required string.
    pub fn required_string(&self, key: &str) -> Result<String> {
        self.optional_string(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional float, rejecting NaN and infinity.
    pub fn optional_f64(&self, key: &str) -> Result<Option<f64>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => match value.as_f64() {
                Some(number) if number.is_finite() => Ok(Some(number)),
                _ => Err(Error::InvalidArguments(format!(
                    "'{key}' must be a finite number"
                ))),
            },
        }
    }

    /// Required float.
    pub fn required_f64(&self, key: &str) -> Result<f64> {
        self.optional_f64(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional boolean.
    pub fn optional_bool(&self, key: &str) -> Result<Option<bool>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(Error::InvalidArguments(format!(
                "'{key}' must be a boolean"
            ))),
        }
    }

    /// Required boolean.
    pub fn required_bool(&self, key: &str) -> Result<bool> {
        self.optional_bool(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional unsigned integer.
    pub fn optional_u64(&self, key: &str) -> Result<Option<u64>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                Error::InvalidArguments(format!("'{key}' must be a non-negative integer"))
            }),
        }
    }

    /// Required unsigned integer.
    pub fn required_u64(&self, key: &str) -> Result<u64> {
        self.optional_u64(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }

    /// Optional array of non-empty strings.
    pub fn optional_string_list(&self, key: &str) -> Result<Option<Vec<String>>> {
        match self.map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(items)) => {
                if items.is_empty() {
                    return Err(Error::InvalidArguments(format!(
                        "'{key}' must contain at least one item"
                    )));
                }
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let text = item.as_str().ok_or_else(|| {
                        Error::InvalidArguments(format!("'{key}' must contain only strings"))
                    })?;
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        return Err(Error::InvalidArguments(format!(
                            "'{key}' must not contain empty strings"
                        )));
                    }
                    if trimmed.contains('\0') {
                        return Err(Error::InvalidArguments(format!(
                            "'{key}' must not contain NUL bytes"
                        )));
                    }
                    out.push(trimmed.to_string());
                }
                Ok(Some(out))
            }
            Some(_) => Err(Error::InvalidArguments(format!(
                "'{key}' must be an array of strings"
            ))),
        }
    }

    /// Required string list.
    pub fn required_string_list(&self, key: &str) -> Result<Vec<String>> {
        self.optional_string_list(key)?
            .ok_or_else(|| Error::InvalidArguments(format!("'{key}' is required")))
    }
}

/// Validate a relative Moonraker file path.
///
/// Rejects absolute paths, `..` traversal, backslashes, and control characters.
/// Moonraker itself is the final authority, but rejecting bad paths here keeps
/// audit records meaningful and prevents accidental cross-root access.
pub fn validate_relative_path(path: &str, what: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidArguments(format!("{what} must not be empty")));
    }
    if trimmed.len() > 512 {
        return Err(Error::InvalidArguments(format!("{what} is too long")));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(Error::InvalidArguments(format!(
            "{what} must be relative, not absolute"
        )));
    }
    if trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed.contains('\n')
        || trimmed.contains('\r')
    {
        return Err(Error::InvalidArguments(format!(
            "{what} contains invalid characters"
        )));
    }
    if trimmed
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(Error::InvalidArguments(format!(
            "{what} must not contain empty, '.' or '..' path segments"
        )));
    }
    Ok(trimmed.to_string())
}

/// Validate a Moonraker file root name.
pub fn validate_root(root: &str) -> Result<String> {
    let root = root.trim();
    if root.is_empty()
        || root.len() > 64
        || !root
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(Error::InvalidArguments(
            "root must contain only letters, digits, '_' and '-'".to_string(),
        ));
    }
    Ok(root.to_string())
}

/// Validate a Klipper object name (heater, fan, macro, ...).
pub fn validate_object_name(name: &str, what: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 96 {
        return Err(Error::InvalidArguments(format!("{what} is invalid")));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ' ' | ':' | '/'))
    {
        return Err(Error::InvalidArguments(format!(
            "{what} contains unsupported characters"
        )));
    }
    Ok(name.to_string())
}

/// Quote a string for safe inclusion in a Klipper G-code command.
#[must_use]
pub fn gcode_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_unknown_keys() {
        let err = Args::parse(Some(json!({"tmeperature": 200})), &["temperature"]).unwrap_err();
        assert!(err.to_string().contains("unknown argument 'tmeperature'"));
    }

    #[test]
    fn accepts_null_arguments() {
        let args = Args::parse(Some(Value::Null), &["x"]).unwrap();
        assert!(!args.contains("x"));
    }

    #[test]
    fn validates_numbers() {
        let args = Args::parse(Some(json!({"t": 200})), &["t"]).unwrap();
        assert!((args.required_f64("t").unwrap() - 200.0).abs() < f64::EPSILON);
        let args = Args::parse(Some(json!({"t": "hot"})), &["t"]).unwrap();
        let err = args.required_f64("t").unwrap_err();
        assert!(err.to_string().contains("finite number"));
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(validate_relative_path("../secret.gcode", "filename").is_err());
        assert!(validate_relative_path("/etc/passwd", "filename").is_err());
        assert!(validate_relative_path("a//b.gcode", "filename").is_err());
        assert!(validate_relative_path("parts/benchy.gcode", "filename").is_ok());
    }

    #[test]
    fn validates_object_names() {
        assert!(validate_object_name("heater_generic chamber", "heater").is_ok());
        assert!(validate_object_name("extruder\nM112", "heater").is_err());
        assert!(validate_object_name("", "heater").is_err());
    }
}
