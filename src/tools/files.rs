//! G-code file tools.
//!
//! File deletion is the only destructive file operation exposed. It is limited
//! to the `gcodes` root and always requires the double-confirmation flow.
//! Uploads and configuration writes are intentionally out of scope for v0.1.

use serde_json::{json, Value};

use crate::{
    safety::{Capability, Risk},
    tools::{
        args::{validate_relative_path, validate_root, Args},
        json_result, no_summary, schema_object, Gate, ToolContext, ToolSpec,
    },
};

/// File tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_files_list",
            title: "List printer files",
            description: "List files and directories under a Moonraker file root (default \
                          'gcodes'). Read-only; paths are relative to the root.",
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
                          filament usage, layer height, thumbnails, and slicer information when \
                          available. The filename is relative to the 'gcodes' root.",
            allowed_args: &["filename"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: metadata_schema,
            handler: |ctx, args| Box::pin(file_metadata(ctx, args)),
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
) -> Result<rust_mcp_sdk::schema::CallToolResult, crate::Error> {
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

async fn file_metadata(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult, crate::Error> {
    let args = Args::parse(Some(arguments), &["filename"])?;
    let filename = validate_relative_path(&args.required_string("filename")?, "filename")?;
    let value = ctx
        .client
        .call_value("server.files.metadata", Some(json!({"filename": filename})))
        .await?;
    Ok(json_result(
        &crate::safety::redact::redact_value(&value),
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

async fn file_delete(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult, crate::Error> {
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
