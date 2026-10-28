//! Job queue tools.
//!
//! Adding, pausing, and starting the queue are reversible. Removing queued jobs
//! is approval-gated because `all=true` can wipe a prepared production queue.

use serde_json::{json, Value};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{
        args::{validate_relative_path, Args},
        json_result, no_summary, schema_object, Gate, ToolContext, ToolSpec,
    },
};

/// Queue tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_job_queue_status",
            title: "Job queue status",
            description: "Return the job queue state and all queued jobs.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(queue_status(ctx)),
        },
        ToolSpec {
            name: "moonraker_job_queue_add",
            title: "Add jobs to the queue",
            description: "Append one or more G-code files (relative to the gcodes root) to the \
                          job queue. Does not clear the existing queue.",
            allowed_args: &["filenames"],
            capability: Capability::Mutation,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: summarize_add,
            schema: add_schema,
            handler: |ctx, args| Box::pin(queue_add(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_job_queue_start",
            title: "Start the job queue",
            description: "Start/resume the job queue so the next queued job can load when Klipper \
                          is ready.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: |_| "Start the job queue.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(queue_action(ctx, "start")),
        },
        ToolSpec {
            name: "moonraker_job_queue_pause",
            title: "Pause the job queue",
            description: "Pause the job queue so no further jobs load after the current job.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: |_| "Pause the job queue.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(queue_action(ctx, "pause")),
        },
        ToolSpec {
            name: "moonraker_job_queue_jump",
            title: "Jump a job to the front",
            description: "Move a queued job to the front of the queue.",
            allowed_args: &["job_id"],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: summarize_jump,
            schema: jump_schema,
            handler: |ctx, args| Box::pin(queue_jump(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_job_queue_remove",
            title: "Remove queued jobs",
            description:
                "DESTRUCTIVE: remove one or more jobs from the queue, or clear the entire \
                          queue with all=true. Requires double confirmation through \
                          moonraker_confirm_action.",
            allowed_args: &["job_ids", "all"],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: summarize_remove,
            schema: remove_schema,
            handler: |ctx, args| Box::pin(queue_remove(ctx, args)),
        },
    ]
}

async fn queue_status(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let value = ctx
        .client
        .call_value("server.job_queue.status", None)
        .await?;
    Ok(json_result(&value, None))
}

fn add_schema() -> Value {
    schema_object(
        json!({
            "filenames": {
                "type": "array",
                "items": {"type": "string"},
                "minItems": 1,
                "maxItems": 100,
                "description": "G-code filenames relative to the gcodes root. Every file must exist."
            }
        }),
        &["filenames"],
    )
}

fn summarize_add(args: &Value) -> String {
    let count = args
        .get("filenames")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    format!("Add {count} job(s) to the end of the job queue.")
}

async fn queue_add(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["filenames"])?;
    let filenames = args.required_string_list("filenames")?;
    if filenames.len() > 100 {
        return Err(Error::InvalidArguments(
            "at most 100 files may be queued at once".to_string(),
        ));
    }
    let filenames = filenames
        .iter()
        .map(|name| validate_relative_path(name, "filename"))
        .collect::<Result<Vec<_>>>()?;

    let value = ctx
        .client
        .call_value(
            "server.job_queue.post_job",
            Some(json!({"filenames": filenames, "reset": false})),
        )
        .await?;
    Ok(json_result(&value, None))
}

async fn queue_action(
    ctx: &ToolContext,
    action: &str,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let method = format!("server.job_queue.{action}");
    let value = ctx.client.call_value(&method, None).await?;
    Ok(json_result(&value, None))
}

fn jump_schema() -> Value {
    schema_object(
        json!({
            "job_id": {
                "type": "string",
                "description": "Job id from moonraker_job_queue_status."
            }
        }),
        &["job_id"],
    )
}

fn summarize_jump(args: &Value) -> String {
    format!(
        "Jump job {} to the front of the queue.",
        args.get("job_id")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
    )
}

async fn queue_jump(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["job_id"])?;
    let job_id = args.required_string("job_id")?;
    if job_id.len() > 64 || !job_id.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(Error::InvalidArguments("invalid job_id".to_string()));
    }
    let value = ctx
        .client
        .call_value("server.job_queue.jump", Some(json!({"job_id": job_id})))
        .await?;
    Ok(json_result(&value, None))
}

fn remove_schema() -> Value {
    schema_object(
        json!({
            "job_ids": {
                "type": "array",
                "items": {"type": "string"},
                "maxItems": 100,
                "description": "Job ids to remove. Required unless all=true."
            },
            "all": {
                "type": "boolean",
                "default": false,
                "description": "Remove every job in the queue."
            }
        }),
        &[],
    )
}

fn summarize_remove(args: &Value) -> String {
    if args.get("all").and_then(Value::as_bool) == Some(true) {
        return "Remove ALL jobs from the job queue.".to_string();
    }
    let count = args
        .get("job_ids")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let ids = args
        .get("job_ids")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    format!("Remove {count} queued job(s): {ids}")
}

async fn queue_remove(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["job_ids", "all"])?;
    let all = args.optional_bool("all")?.unwrap_or(false);
    let job_ids = args.optional_string_list("job_ids")?;

    if !all && job_ids.is_none() {
        return Err(Error::InvalidArguments(
            "provide job_ids, or set all=true to clear the queue".to_string(),
        ));
    }
    if all && job_ids.is_some() {
        return Err(Error::InvalidArguments(
            "provide either job_ids or all=true, not both".to_string(),
        ));
    }
    if let Some(ids) = &job_ids {
        if ids.len() > 100 {
            return Err(Error::InvalidArguments(
                "at most 100 job ids may be removed at once".to_string(),
            ));
        }
        for id in ids {
            if id.len() > 64 || !id.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return Err(Error::InvalidArguments(format!("invalid job id '{id}'")));
            }
        }
    }

    let params = if all {
        json!({"all": true})
    } else {
        json!({"job_ids": job_ids})
    };
    let value = ctx
        .client
        .call_value("server.job_queue.delete_job", Some(params))
        .await?;
    Ok(json_result(&value, None))
}
