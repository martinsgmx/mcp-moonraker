//! Print job control and printer lifecycle tools.
//!
//! Starting or cancelling a print, restarting Klipper, and arbitrary G-code are
//! all approval-gated. Emergency stop is the one exception: it is never delayed
//! by an approval prompt, because delaying it defeats its purpose.

use serde_json::{json, Map, Value};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{
        args::{validate_relative_path, Args},
        json_result, no_summary, query_objects, schema_object, Gate, ToolContext, ToolSpec,
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
            name: "moonraker_print_preflight",
            title: "Preflight a print start",
            description: "Read-only consolidation of the checks needed before starting a print: \
                           Klippy state, active job, job queue, temperatures, file existence and \
                           metadata, and local policy caps. Returns `ready_to_prepare_start` with \
                           blocking reasons. It never starts a print or changes any state.",
            allowed_args: &["filename"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: filename_schema,
            handler: |ctx, args| Box::pin(print_preflight(ctx, args)),
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

/// Whether a print can be started from the given `print_stats.state`.
fn print_state_is_idle(state: &str) -> bool {
    matches!(
        state.to_ascii_lowercase().as_str(),
        "idle" | "complete" | "cancelled" | "error"
    )
}

/// Add one preflight check and route its detail into blockers or warnings.
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn push_check(
    checks: &mut Vec<Value>,
    blockers: &mut Vec<String>,
    warnings: &mut Vec<String>,
    name: &str,
    ok: bool,
    blocking: bool,
    detail: String,
    evidence: Value,
) {
    let status = if ok {
        "green"
    } else if blocking {
        "red"
    } else {
        "yellow"
    };
    checks.push(json!({
        "check": name,
        "status": status,
        "blocking": !ok && blocking,
        "detail": detail,
        "evidence": evidence,
    }));
    if !ok {
        if blocking {
            blockers.push(detail);
        } else {
            warnings.push(detail);
        }
    }
}

async fn print_preflight(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["filename"])?;
    let filename = validate_relative_path(&args.required_string("filename")?, "filename")?;

    let mut checks = Vec::new();
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    // Klippy must be connected and ready to accept a print.
    let server = ctx.client.call_value("server.info", None).await?;
    let klippy_state = server
        .get("klippy_state")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let klippy_ok = klippy_state.eq_ignore_ascii_case("ready");
    push_check(
        &mut checks,
        &mut blockers,
        &mut warnings,
        "klippy_ready",
        klippy_ok,
        true,
        format!("Klippy state is '{klippy_state}' (expected 'ready')."),
        json!({"klippy_state": klippy_state}),
    );

    // An active job would be replaced by a new print.
    let stats_value = query_objects(
        ctx,
        [("print_stats".to_string(), json!(["state", "filename"]))]
            .into_iter()
            .collect(),
    )
    .await?;
    let stats = stats_value
        .get("status")
        .and_then(|status| status.get("print_stats"))
        .cloned()
        .unwrap_or(Value::Null);
    let print_state = stats
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let active_file = stats
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let state_ok = print_state_is_idle(&print_state);
    push_check(
        &mut checks,
        &mut blockers,
        &mut warnings,
        "no_active_job",
        state_ok,
        true,
        format!(
            "print state is '{print_state}'{}; starting now would replace the active job.",
            if active_file.is_empty() {
                String::new()
            } else {
                format!(" with '{active_file}' loaded")
            }
        ),
        json!({"state": print_state, "filename": active_file}),
    );

    // The file must exist and ideally have usable metadata.
    let metadata = match ctx
        .client
        .call_value("server.files.metadata", Some(json!({"filename": filename})))
        .await
    {
        Ok(value) => Some(value),
        Err(Error::Api { .. }) => None,
        Err(err) => return Err(err),
    };
    let file_found = metadata.is_some();
    let (summary, metadata_warnings) = metadata.as_ref().map_or_else(
        || {
            (
                json!({"source": "moonraker_metadata", "available": false}),
                Vec::new(),
            )
        },
        crate::tools::files::metadata_summary,
    );
    push_check(
        &mut checks,
        &mut blockers,
        &mut warnings,
        "file_exists",
        file_found,
        true,
        if file_found {
            format!("'{filename}' is present in the gcodes root.")
        } else {
            format!("'{filename}' was not found in the gcodes root.")
        },
        json!({"exists": file_found, "summary": summary}),
    );
    warnings.extend(metadata_warnings);

    // Queue state is informational: a paused queue does not prevent this print.
    let queue = ctx
        .client
        .call_value("server.job_queue.status", None)
        .await
        .unwrap_or(Value::Null);
    let queue_state = queue
        .get("queue_state")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let queued_jobs = queue
        .get("queued_jobs")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    push_check(
        &mut checks,
        &mut blockers,
        &mut warnings,
        "job_queue",
        !queue_state.eq_ignore_ascii_case("paused"),
        false,
        format!("job queue state is '{queue_state}' with {queued_jobs} queued job(s)."),
        json!({"queue_state": queue_state, "queued_jobs": queued_jobs}),
    );

    // Temperatures are informational; Klipper applies the file's own targets.
    let mut temp_query = Map::new();
    temp_query.insert("extruder".into(), json!(["temperature", "target"]));
    temp_query.insert("heater_bed".into(), json!(["temperature", "target"]));
    let temps = query_objects(ctx, temp_query).await;
    let temps_ok = temps.is_ok();
    let temps_value = temps.unwrap_or(Value::Null);
    push_check(
        &mut checks,
        &mut blockers,
        &mut warnings,
        "temperatures",
        temps_ok,
        false,
        if temps_ok {
            "Current temperatures are informational; the G-code sets its own targets.".to_string()
        } else {
            "Printer temperatures could not be queried.".to_string()
        },
        temps_value.get("status").cloned().unwrap_or(Value::Null),
    );

    // Local policy can deny print start regardless of printer state.
    let policy = ctx.policy.summary();
    let policy_ok = policy["read_only"] != json!(true) && policy["allow_mutations"] != json!(false);
    push_check(
        &mut checks,
        &mut blockers,
        &mut warnings,
        "policy_allows_start",
        policy_ok,
        true,
        if policy_ok {
            "Local policy allows mutation tools (print start requires approval).".to_string()
        } else {
            "Local policy denies mutations; print start is not available.".to_string()
        },
        policy.clone(),
    );

    let ready = blockers.is_empty();
    Ok(crate::tools::json_result(
        &json!({
            "filename": filename,
            "checks": checks,
            "blocking_reasons": blockers,
            "warnings": warnings,
            "active_job": {"state": print_state, "filename": active_file},
            "file": {
                "exists": file_found,
                "metadata": crate::safety::redact::redact_value(
                    &metadata.unwrap_or(Value::Null)
                ),
            },
            "policy": policy,
            "ready_to_prepare_start": ready,
            "next_step": if ready {
                json!(format!("moonraker_print_start with filename '{filename}'"))
            } else {
                Value::Null
            },
            "note": "Read-only preflight: no print was started and no state was changed.",
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_states_allow_a_new_print() {
        for state in ["Idle", "idle", "COMPLETE", "cancelled", "error"] {
            assert!(print_state_is_idle(state));
        }
        for state in ["printing", "Paused", "shutdown", "unknown"] {
            assert!(!print_state_is_idle(state));
        }
    }

    #[test]
    fn checks_route_details_to_blockers_or_warnings() {
        let mut checks = Vec::new();
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        push_check(
            &mut checks,
            &mut blockers,
            &mut warnings,
            "file_exists",
            false,
            true,
            "missing".into(),
            json!({}),
        );
        push_check(
            &mut checks,
            &mut blockers,
            &mut warnings,
            "job_queue",
            false,
            false,
            "paused".into(),
            json!({}),
        );
        assert_eq!(checks[0]["status"], "red");
        assert_eq!(checks[1]["status"], "yellow");
        assert_eq!(blockers, vec!["missing"]);
        assert_eq!(warnings, vec!["paused"]);
    }
}
