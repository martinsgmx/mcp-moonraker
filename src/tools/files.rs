//! G-code file tools.
//!
//! Deleting a file and uploading with `overwrite=true` are the destructive
//! file operations exposed. Both are limited to the `gcodes` root and go
//! through the double-confirmation flow. Uploads send a SHA-256 checksum that
//! Moonraker verifies itself, so a corrupt transfer is rejected server-side.

use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{
        args::{validate_relative_path, validate_root, Args},
        json_result, no_summary, schema_object, Gate, ToolContext, ToolSpec,
    },
};

/// Supported G-code extensions (Moonraker's `VALID_GCODE_EXTS`, minus `.ufp`
/// which Moonraker unpacks and renames during upload).
const GCODE_EXTENSIONS: &[&str] = &[".gcode", ".g", ".gco", ".nc"];

/// File tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_files_list",
            title: "List printer files",
            description: "List files and directories under a Moonraker file root (default \
                           'gcodes'). Read-only; paths are relative to the root. For a file's \
                           SHA-256, call moonraker_file_verify.",
            allowed_args: &["root", "path", "limit"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: list_schema,
            handler: |ctx, args| Box::pin(files_list(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_file_metadata",
            title: "G-code file metadata",
            description: "Return Moonraker's metadata for a G-code file: estimated print time, \
                           filament usage, layer count, thumbnails, and slicer information when \
                           available. Includes a normalized summary and warnings about missing \
                           or inconsistent fields. The filename is relative to the 'gcodes' root.",
            allowed_args: &["filename", "include_checksum"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: metadata_schema,
            handler: |ctx, args| Box::pin(file_metadata(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_file_verify",
            title: "Verify a G-code file",
            description: "Read-only transfer verification: download a file from the 'gcodes' \
                           root, compute its size and SHA-256, and optionally compare them with \
                           expected values. Returns `verified`, per-field `checks`, and any \
                           metadata `warnings`. Never modifies the printer.",
            allowed_args: &["filename", "expected_sha256", "expected_size"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: verify_schema,
            handler: |ctx, args| Box::pin(file_verify(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_file_upload",
            title: "Upload a G-code file",
            description: "Upload base64-encoded G-code into the 'gcodes' root. A new file is a \
                           reversible mutation; setting overwrite=true is destructive and \
                           requires double confirmation through moonraker_confirm_action. The \
                           SHA-256 is sent to Moonraker, which rejects a corrupt transfer. \
                           Uploading never starts a print; use moonraker_print_preflight and \
                           moonraker_print_start for that.",
            allowed_args: &["filename", "data_base64", "root", "overwrite"],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: summarize_upload,
            schema: upload_schema,
            handler: |ctx, args| Box::pin(file_upload(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_file_delete",
            title: "Delete a G-code file",
            description: "DESTRUCTIVE: delete a file from the 'gcodes' root. The first call \
                           returns an approval prompt; a second call to moonraker_confirm_action \
                           with the approval id, confirmation phrase, and acknowledge=true is \
                           required. Configuration files cannot be deleted.",
            allowed_args: &["filename"],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: summarize_delete,
            schema: metadata_schema,
            handler: |ctx, args| Box::pin(file_delete(ctx, args)),
        },
    ]
}

fn list_schema() -> Value {
    schema_object(
        json!({
            "root": {
                "type": "string",
                "default": "gcodes",
                "description": "Moonraker file root, for example 'gcodes', 'config', 'logs', 'config_examples', or 'docs'."
            },
            "path": {
                "type": "string",
                "description": "Optional subdirectory relative to the root."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 2000,
                "default": 500,
                "description": "Maximum entries to return."
            }
        }),
        &[],
    )
}

async fn files_list(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["root", "path", "limit"])?;
    let root = validate_root(
        &args
            .optional_string("root")?
            .unwrap_or_else(|| "gcodes".into()),
    )?;
    let path = args
        .optional_string("path")?
        .map(|path| validate_relative_path(&path, "path"))
        .transpose()?;
    let limit = args.optional_u64("limit")?.unwrap_or(500).clamp(1, 2000) as usize;

    let mut params = json!({"root": root});
    if let Some(path) = &path {
        params["path"] = json!(path);
    }
    let value = ctx
        .client
        .call_value("server.files.list", Some(params))
        .await?;
    let entries = value.as_array().cloned().unwrap_or_default();
    let total = entries.len();
    let entries = entries.into_iter().take(limit).collect::<Vec<_>>();

    Ok(json_result(
        &json!({
            "root": root,
            "path": path,
            "total": total,
            "returned": entries.len(),
            "entries": entries,
            "verification_hint": "Call moonraker_file_verify with a filename for sha256 and \
                                  post-transfer verification.",
        }),
        None,
    ))
}

fn metadata_schema() -> Value {
    schema_object(
        json!({
            "filename": {
                "type": "string",
                "description": "Path relative to the 'gcodes' root, for example 'parts/benchy.gcode'."
            }
        }),
        &["filename"],
    )
}

fn verify_schema() -> Value {
    schema_object(
        json!({
            "filename": {
                "type": "string",
                "description": "Path relative to the 'gcodes' root."
            },
            "expected_sha256": {
                "type": "string",
                "description": "Optional 64-character hex SHA-256 to compare against the downloaded file."
            },
            "expected_size": {
                "type": "integer",
                "minimum": 0,
                "description": "Optional exact file size in bytes."
            }
        }),
        &["filename"],
    )
}

fn upload_schema() -> Value {
    schema_object(
        json!({
            "filename": {
                "type": "string",
                "description": "Destination path relative to the 'gcodes' root, for example 'parts/benchy.gcode'. Must use a supported G-code extension."
            },
            "data_base64": {
                "type": "string",
                "description": "Base64-encoded file bytes. Whitespace is ignored. Limited by moonraker.max_upload_bytes."
            },
            "root": {
                "type": "string",
                "default": "gcodes",
                "description": "Must be 'gcodes'; uploads to other roots are rejected."
            },
            "overwrite": {
                "type": "boolean",
                "default": false,
                "description": "Allow replacing an existing file. Requires double confirmation."
            }
        }),
        &["filename", "data_base64"],
    )
}

fn summarize_upload(args: &Value) -> String {
    let filename = args
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if overwrite {
        format!(
            "Upload and OVERWRITE '{filename}' in the gcodes root. An existing file with that \
             name will be replaced."
        )
    } else {
        format!("Upload a new G-code file '{filename}' to the gcodes root.")
    }
}

async fn file_metadata(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["filename", "include_checksum"])?;
    let filename = validate_relative_path(&args.required_string("filename")?, "filename")?;
    let include_checksum = args.optional_bool("include_checksum")?.unwrap_or(false);
    let value = ctx
        .client
        .call_value("server.files.metadata", Some(json!({"filename": filename})))
        .await?;
    let (summary, mut warnings) = metadata_summary(&value);

    let checksum = if include_checksum {
        let (sha256, size) = file_hash(ctx, &filename).await?;
        if let Some(metadata_size) = value.get("size").and_then(Value::as_u64) {
            if metadata_size != size as u64 {
                warnings.push(format!(
                    "metadata size ({metadata_size} bytes) disagrees with the downloaded file \
                     ({size} bytes)"
                ));
            }
        }
        json!({
            "algorithm": "sha256",
            "sha256": sha256,
            "size_bytes": size,
            "source": "downloaded from gcodes",
        })
    } else {
        Value::Null
    };

    Ok(json_result(
        &json!({
            "filename": filename,
            "metadata": crate::safety::redact::redact_value(&value),
            "summary": summary,
            "warnings": warnings,
            "checksum": checksum,
            "verification_hint": if include_checksum {
                Value::Null
            } else {
                json!("Set include_checksum=true, or call moonraker_file_verify, to get a sha256.")
            },
        }),
        None,
    ))
}

async fn file_verify(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(
        Some(arguments),
        &["filename", "expected_sha256", "expected_size"],
    )?;
    let filename = validate_relative_path(&args.required_string("filename")?, "filename")?;
    let expected_sha256 = args
        .optional_string("expected_sha256")?
        .map(|value| validate_sha256(&value))
        .transpose()?;
    let expected_size = args.optional_u64("expected_size")?;

    let metadata = metadata_or_none(ctx, &filename).await?;
    let (sha256, size) = file_hash(ctx, &filename).await?;
    let (summary, mut warnings) = metadata.as_ref().map_or_else(
        || {
            (
                json!({"source": "moonraker_metadata", "available": false}),
                vec!["Moonraker has no parsed metadata for this file yet.".to_string()],
            )
        },
        metadata_summary,
    );

    let mut checks = Map::new();
    let mut verified = true;
    if let Some(expected) = &expected_sha256 {
        let matches = expected == &sha256;
        checks.insert(
            "sha256".into(),
            json!({"expected": expected, "actual": sha256, "match": matches}),
        );
        if !matches {
            warnings.push("sha256 does not match the expected value.".to_string());
        }
        verified &= matches;
    }
    if let Some(expected) = expected_size {
        let matches = expected == size as u64;
        checks.insert(
            "size_bytes".into(),
            json!({"expected": expected, "actual": size, "match": matches}),
        );
        if !matches {
            warnings.push(format!(
                "size does not match the expected value ({expected} vs {size} bytes)."
            ));
        }
        verified &= matches;
    }
    if let Some(metadata_size) = metadata
        .as_ref()
        .and_then(|metadata| metadata.get("size"))
        .and_then(Value::as_u64)
    {
        let matches = metadata_size == size as u64;
        checks.insert(
            "metadata_size_bytes".into(),
            json!({"expected": metadata_size, "actual": size, "match": matches}),
        );
        if !matches {
            warnings.push(format!(
                "metadata size ({metadata_size} bytes) disagrees with the downloaded file \
                 ({size} bytes)."
            ));
        }
        verified &= matches;
    }

    let metadata_out = metadata
        .as_ref()
        .map_or(Value::Null, crate::safety::redact::redact_value);
    Ok(json_result(
        &json!({
            "filename": filename,
            "root": "gcodes",
            "size_bytes": size,
            "sha256": sha256,
            "metadata": metadata_out,
            "summary": summary,
            "checks": checks,
            "warnings": warnings,
            "verified": verified,
        }),
        None,
    ))
}

fn summarize_delete(args: &Value) -> String {
    let filename = args
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    format!(
        "Delete G-code file '{filename}' from the printer's gcodes root. This cannot be undone."
    )
}

async fn file_upload(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(
        Some(arguments),
        &["filename", "data_base64", "root", "overwrite"],
    )?;
    let root = validate_root(
        &args
            .optional_string("root")?
            .unwrap_or_else(|| "gcodes".into()),
    )?;
    if root != "gcodes" {
        return Err(Error::InvalidArguments(
            "uploads are only allowed to the 'gcodes' root".to_string(),
        ));
    }
    let filename = validate_gcode_filename(&args.required_string("filename")?)?;
    let overwrite = args.optional_bool("overwrite")?.unwrap_or(false);
    let encoded = args.required_string("data_base64")?;
    let bytes = decode_base64(&encoded, ctx.config.moonraker.max_upload_bytes)?;
    let bytes_len = bytes.len();
    let digest = Sha256::digest(&bytes);
    let sha256 = hex::encode(digest);

    // Re-check existence at execution time: the approval may be minutes old,
    // and a non-overwrite upload must never silently replace a file.
    let exists = file_exists(ctx, &filename).await?;
    if exists && !overwrite {
        return Err(Error::InvalidArguments(format!(
            "'{filename}' already exists; set overwrite=true to replace it (requires approval)"
        )));
    }

    let result = ctx
        .client
        .upload_file("gcodes", &filename, bytes, &sha256)
        .await?;

    Ok(json_result(
        &json!({
            "uploaded": filename,
            "root": "gcodes",
            "bytes": bytes_len,
            "sha256": sha256,
            "overwrote": exists,
            "checksum_verified_by_moonraker": true,
            "result": crate::safety::redact::redact_value(&result),
        }),
        None,
    ))
}

async fn file_delete(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["filename"])?;
    let filename = validate_relative_path(&args.required_string("filename")?, "filename")?;

    // Re-check existence at execution time; the approval may be minutes old.
    ctx.client
        .call_value("server.files.metadata", Some(json!({"filename": filename})))
        .await?;

    let value = ctx
        .client
        .call_value(
            "server.files.delete_file",
            Some(json!({"path": format!("gcodes/{filename}")})),
        )
        .await?;

    Ok(json_result(
        &json!({
            "deleted": filename,
            "result": value,
        }),
        Some("File deleted."),
    ))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Normalize a summary from Moonraker's metadata and flag unreliable fields.
pub(crate) fn metadata_summary(metadata: &Value) -> (Value, Vec<String>) {
    let mut warnings = Vec::new();
    if metadata.as_object().is_none_or(Map::is_empty) {
        warnings.push(
            "Moonraker has no parsed metadata for this file yet; estimated time, filament, and \
             layers are unavailable."
                .to_string(),
        );
    }
    let time = positive_number(metadata, "estimated_time");
    if time.is_none() {
        warnings.push("estimated_time is missing or non-positive.".to_string());
    }
    let filament = positive_number(metadata, "filament_total");
    if filament.is_none() {
        warnings.push("filament_total is missing or non-positive.".to_string());
    }
    let layers = positive_number(metadata, "layer_count");
    if layers.is_none() {
        warnings.push("layer_count is missing or non-positive.".to_string());
    }
    let layer_height = positive_number(metadata, "layer_height");
    let first_layer_height = positive_number(metadata, "first_layer_height");
    if let (Some(first), Some(layer)) = (first_layer_height, layer_height) {
        if first > layer * 1.05 {
            warnings.push(format!(
                "first_layer_height ({first}) is much larger than layer_height ({layer})."
            ));
        }
    }
    if let Some(weight) = metadata_number(metadata, "filament_weight_total") {
        if weight <= 0.0 && filament.is_some() {
            warnings.push("filament_weight_total is non-positive.".to_string());
        }
    }

    let summary = json!({
        "source": "moonraker_metadata",
        "estimated_time_secs": time,
        "filament_total_mm": filament,
        "filament_weight_total_g": positive_number(metadata, "filament_weight_total"),
        "layer_count": positive_u64(layers),
        "layer_height": layer_height,
        "first_layer_height": first_layer_height,
        "slicer": metadata.get("slicer").and_then(Value::as_str),
        "slicer_version": metadata.get("slicer_version").and_then(Value::as_str),
    });
    (summary, warnings)
}

fn metadata_number(metadata: &Value, key: &str) -> Option<f64> {
    metadata
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn positive_number(metadata: &Value, key: &str) -> Option<f64> {
    metadata_number(metadata, key).filter(|value| *value > 0.0)
}

/// A positive count from metadata, rounded down to a whole number.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn positive_u64(value: Option<f64>) -> Option<u64> {
    value.map(|value| value as u64)
}

/// Moonraker metadata for a file, or `None` when it has not been analyzed.
async fn metadata_or_none(ctx: &ToolContext, filename: &str) -> Result<Option<Value>> {
    match ctx
        .client
        .call_value("server.files.metadata", Some(json!({"filename": filename})))
        .await
    {
        Ok(value) => Ok(Some(value)),
        Err(Error::Api { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

async fn file_exists(ctx: &ToolContext, filename: &str) -> Result<bool> {
    Ok(metadata_or_none(ctx, filename).await?.is_some())
}

/// Download a G-code file and return `(sha256_hex, size_bytes)`.
async fn file_hash(ctx: &ToolContext, filename: &str) -> Result<(String, usize)> {
    let bytes = ctx
        .client
        .download_file("gcodes", filename, ctx.config.moonraker.max_upload_bytes)
        .await
        .map_err(|err| match err {
            Error::Api { code: 404, .. } => Error::InvalidArguments(format!(
                "file '{filename}' was not found in the gcodes root"
            )),
            other => other,
        })?;
    Ok((sha256_hex(&bytes), bytes.len()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Decode base64, ignoring whitespace and bounding the decoded size before
/// allocating. Standard alphabet only; URL-safe input is rejected explicitly.
fn decode_base64(encoded: &str, max_bytes: usize) -> Result<Vec<u8>> {
    if encoded.trim().is_empty() {
        return Err(Error::InvalidArguments(
            "data_base64 must not be empty".into(),
        ));
    }
    let max_encoded = max_bytes
        .saturating_mul(4)
        .saturating_div(3)
        .saturating_add(4);
    if encoded.len() > max_encoded {
        return Err(Error::InvalidArguments(format!(
            "data_base64 is too large (decoded limit is {max_bytes} bytes)"
        )));
    }
    let compact: String = encoded
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|err| {
            Error::InvalidArguments(format!("data_base64 is not valid base64: {err}"))
        })?;
    if bytes.is_empty() {
        return Err(Error::InvalidArguments(
            "data_base64 decoded to zero bytes".into(),
        ));
    }
    if bytes.len() > max_bytes {
        return Err(Error::InvalidArguments(format!(
            "decoded file is too large ({} bytes, limit {max_bytes})",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn validate_gcode_filename(name: &str) -> Result<String> {
    let name = validate_relative_path(name, "filename")?;
    let lower = name.to_ascii_lowercase();
    if !GCODE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        return Err(Error::InvalidArguments(
            "filename must use a supported G-code extension: .gcode, .g, .gco, or .nc".into(),
        ));
    }
    Ok(name)
}

fn validate_sha256(value: &str) -> Result<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(Error::InvalidArguments(
            "expected_sha256 must be 64 hexadecimal characters".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_handles_whitespace_and_rejects_garbage() {
        let decoded = decode_base64("aGVs bG8=\n", 16).unwrap();
        assert_eq!(decoded, b"hello");
        assert!(decode_base64("not base64!!", 16).is_err());
        assert!(decode_base64("", 16).is_err());
        assert!(decode_base64("aGVsbG8=", 4).is_err());
    }

    #[test]
    fn gcode_filename_validation() {
        assert!(validate_gcode_filename("parts/benchy.gcode").is_ok());
        assert!(validate_gcode_filename("a.gco").is_ok());
        assert!(validate_gcode_filename("../../secret.gcode").is_err());
        assert!(validate_gcode_filename("notes.txt").is_err());
        assert!(validate_gcode_filename("archive.ufp").is_err());
    }

    #[test]
    fn metadata_warnings_flag_missing_and_inconsistent_fields() {
        let (_, warnings) = metadata_summary(&json!({}));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("no parsed metadata")));

        let (summary, warnings) = metadata_summary(&json!({
            "estimated_time": 2897,
            "filament_total": 1298.53,
            "layer_count": 277,
            "layer_height": 0.2,
            "first_layer_height": 0.4,
        }));
        assert_eq!(summary["estimated_time_secs"], 2897.0);
        assert_eq!(summary["layer_count"], 277);
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("first_layer_height")));

        let (_, warnings) = metadata_summary(&json!({"estimated_time": -1, "filament_total": 0}));
        assert!(warnings.len() >= 3);
    }

    #[test]
    fn sha256_validation() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_ok());
        assert!(validate_sha256("abc").is_err());
        assert!(validate_sha256(&"g".repeat(64)).is_err());
    }

    #[test]
    fn known_sha256_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
