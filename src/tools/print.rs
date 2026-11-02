//! Print job control and printer lifecycle tools.
//!
//! Starting or cancelling a print, restarting Klipper, and arbitrary G-code are
//! all approval-gated. Emergency stop is the one exception: it is never delayed
//! by an approval prompt, because delaying it defeats its purpose.

use serde_json::{json, Value};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{
        args::{validate_relative_path, Args},
        json_result, query_objects, schema_object, Gate, ToolContext, ToolSpec,
    },
};

/// Print tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_print_start",
            title: "Start a print",
            description: "DESTRUCTIVE: start printing a G-code file from the 'gcodes' root. The \
                          file must exist. This can heat the printer and move all axes, so it \
                          requires double confirmation: the first call returns an approval prompt, \
                          then call moonraker_confirm_action with the approval id, phrase, and \
                          acknowledge=true.",
            allowed_args: &["filename"],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: summarize_start,
            schema: filename_schema,
            handler: |ctx, args| Box::pin(print_start(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_print_pause",
            title: "Pause the current print",
            description: "Pause the active print job. Reversible with moonraker_print_resume.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: |_| "Pause the active print.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(print_pause(ctx)),
        },
        ToolSpec {
            name: "moonraker_print_resume",
            title: "Resume the paused print",
            description: "Resume a paused print job.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: |_| "Resume the paused print.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(print_resume(ctx)),
        },
        ToolSpec {
            name: "moonraker_print_cancel",
            title: "Cancel the current print",
            description: "DESTRUCTIVE: cancel the active or paused print. This cannot be undone; \
                          the part will be incomplete. Requires double confirmation through \
                          moonraker_confirm_action.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: |_| "Cancel the current print job. This cannot be undone.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(print_cancel(ctx)),
        },
        ToolSpec {
            name: "moonraker_emergency_stop",
            title: "Emergency stop",
            description: "IMMEDIATE SAFETY ACTION: halt the printer now and put Klipper into the \
                          shutdown state. This bypasses read-only mode and approval prompts by \
                          design. Use it when something is going wrong physically.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::Critical,
            gate: Gate::Emergency,
            summarize: |_| "Emergency stop: immediately halt the printer.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(emergency_stop(ctx)),
        },
        ToolSpec {
            name: "moonraker_klipper_restart",
            title: "Restart Klipper",
            description: "DESTRUCTIVE: perform a Klipper host restart (reload configuration and \
                          restart Klippy). MCUs are not reset. An active print will fail. Requires \
                          double confirmation.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: |_| {
                "Restart Klipper and reload its configuration. An active print will fail."
                    .to_string()
            },
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(klipper_restart(ctx)),
        },
        ToolSpec {
            name: "moonraker_firmware_restart",
            title: "Restart Klipper firmware",
            description: "DESTRUCTIVE: perform a complete Klipper firmware restart, resetting \
                          Klippy and connected MCUs. An active print will fail. Requires double \
                          confirmation.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: |_| {
                "Restart Klipper and all connected MCUs. An active print will fail.".to_string()
            },
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(firmware_restart(ctx)),
        },
        ToolSpec {
            name: "moonraker_raw_gcode",
            title: "Run arbitrary G-code",
            description: "CRITICAL: execute arbitrary Klipper G-code. Disabled by default \
                          (MCP_MOONRAKER_ALLOW_RAW_GCODE=false). When enabled, every call requires \
                          double confirmation. Prefer the structured motion/heater tools; they \
                          validate against live printer state.",
            allowed_args: &["script"],
            capability: Capability::RawGcode,
            risk: Risk::Critical,
            gate: Gate::Policy,
            summarize: summarize_raw,
            schema: raw_schema,
            handler: |ctx, args| Box::pin(raw_gcode(ctx, args)),
        },
    ]
}

fn filename_schema() -> Value {
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

fn summarize_start(args: &Value) -> String {
    let filename = args
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    format!(
        "Start printing '{filename}'. The printer may heat and move; an active job would be \
         replaced."
    )
}

async fn print_start(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["filename"])?;
    let filename = validate_relative_path(&args.required_string("filename")?, "filename")?;

    // Fail before starting if the file does not exist. This runs after the
    // approval gate, so a missing file consumes the approval but deletes or
    // starts nothing.
    ctx.client
        .call_value("server.files.metadata", Some(json!({"filename": filename})))
        .await?;

    let response = ctx
        .client
        .call_value("printer.print.start", Some(json!({"filename": filename})))
        .await?;
    Ok(json_result(
        &json!({"started": filename, "result": response}),
        None,
    ))
}

async fn print_pause(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    ensure_print_state(ctx, &["printing"], "pause").await?;
    let response = ctx.client.call_value("printer.print.pause", None).await?;
    Ok(json_result(
        &json!({"result": response}),
        Some("Print paused."),
    ))
}

async fn print_resume(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    ensure_print_state(ctx, &["paused"], "resume").await?;
    let response = ctx.client.call_value("printer.print.resume", None).await?;
    Ok(json_result(
        &json!({"result": response}),
        Some("Print resumed."),
    ))
}

async fn print_cancel(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    ensure_print_state(ctx, &["printing", "paused"], "cancel").await?;
    let response = ctx.client.call_value("printer.print.cancel", None).await?;
    Ok(json_result(
        &json!({"result": response}),
        Some("Print cancelled."),
    ))
}

async fn emergency_stop(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let response = ctx
        .client
        .call_value("printer.emergency_stop", None)
        .await?;
    Ok(json_result(
        &json!({"result": response, "state": "shutdown"}),
        Some(
            "EMERGENCY STOP SENT. Klipper is in shutdown state. Inspect the machine before \
             restarting.",
        ),
    ))
}

async fn klipper_restart(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let response = ctx.client.call_value("printer.restart", None).await?;
    Ok(json_result(
        &json!({"result": response}),
        Some("Klipper host restart requested."),
    ))
}

async fn firmware_restart(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let response = ctx
        .client
        .call_value("printer.firmware_restart", None)
        .await?;
    Ok(json_result(
        &json!({"result": response}),
        Some("Klipper firmware restart requested."),
    ))
}

fn raw_schema() -> Value {
    schema_object(
        json!({
            "script": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4000,
                "description": "G-code to execute. Multiple commands may be separated by newlines."
            }
        }),
        &["script"],
    )
}

fn summarize_raw(args: &Value) -> String {
    let script = args.get("script").and_then(Value::as_str).unwrap_or("");
    let preview = crate::safety::redact::truncate_for_audit(script, 300);
    format!("Execute arbitrary G-code:\n{preview}")
}

async fn raw_gcode(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["script"])?;
    let script = args.required_string("script")?;
    if script.len() > 4000 {
        return Err(Error::InvalidArguments(
            "script must be at most 4000 characters".to_string(),
        ));
    }
    if script.contains('\0') {
        return Err(Error::InvalidArguments(
            "script must not contain NUL bytes".to_string(),
        ));
    }
    let response = ctx.client.call_gcode_script(&script).await?;
    Ok(json_result(&json!({"result": response}), None))
}

async fn ensure_print_state(ctx: &ToolContext, allowed: &[&str], action: &str) -> Result<()> {
    let value = query_objects(
        ctx,
        [("print_stats".to_string(), json!(["state", "filename"]))]
            .into_iter()
            .collect(),
    )
    .await?;
    let stats = value
        .get("status")
        .and_then(|status| status.get("print_stats"))
        .cloned()
        .unwrap_or(Value::Null);
    let print_state = stats
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if allowed.contains(&print_state) {
        return Ok(());
    }
    Err(Error::InvalidArguments(format!(
        "cannot {action}: print state is '{print_state}' (expected {}). Current file: {}",
        allowed.join(" or "),
        stats
            .get("filename")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or("none")
    )))
}
