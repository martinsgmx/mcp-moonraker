//! Local safety introspection and the approval confirmation tool.
//!
//! These tools never touch the printer. They tell an agent what the local policy
//! allows, what approvals are pending, and how to complete or cancel one.

// Tool handlers share one async signature so the registry can treat them
// uniformly. These particular handlers have no awaits.
#![allow(clippy::unused_async)]

use serde_json::{json, Value};

use crate::{
    error::{Error, Result},
    safety::{ApprovalStatus, Capability, Risk},
    tools::{args::Args, json_result, no_summary, schema_object, Gate, ToolContext, ToolSpec},
};

/// Safety tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_policy_info",
            title: "Local safety policy",
            description: "Return the MCP server's local safety policy: read-only and mutation \
                          switches, temperature limits, approval mode, and whether raw G-code and \
                          host administration are enabled. Contains no secrets.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Internal,
            summarize: no_summary,
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(policy_info(ctx)),
        },
        ToolSpec {
            name: "moonraker_pending_approvals",
            title: "Pending destructive approvals",
            description: "List prepared destructive actions that are still waiting for confirmation. \
                          The agent can use this after a restart or to recover an approval id.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Internal,
            summarize: no_summary,
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(pending_approvals(ctx)),
        },
        ToolSpec {
            name: "moonraker_cancel_approval",
            title: "Cancel a prepared action",
            description: "Cancel a pending approval so it can never be executed. Safe and \
                          immediate; does not affect the printer.",
            allowed_args: &["approval_id"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Internal,
            summarize: no_summary,
            schema: approval_id_schema,
            handler: |ctx, args| Box::pin(cancel_approval(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_confirm_action",
            title: "Confirm and execute a prepared action",
            description: "Second step of the double-confirmation flow. Pass the approval id from \
                          the prepare response, the confirmation phrase shown to the user (in in-band \
                          mode), and acknowledge=true. The server executes the exact tool and \
                          arguments that were prepared; it never accepts a fresh action here. Each \
                          approval is single-use.",
            allowed_args: &["approval_id", "confirmation_phrase", "acknowledge"],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Confirm,
            summarize: no_summary,
            schema: confirm_schema,
            handler: |_ctx, _args| Box::pin(confirm_placeholder()),
        },
    ]
}

async fn policy_info(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let mode = ctx.config.approval.mode;
    let approvals = json!({
        "mode": mode,
        "ttl_secs": ctx.config.approval.ttl_secs,
        "requires_human": ctx.approvals.requires_human(),
        "requires_phrase": ctx.approvals.requires_phrase(),
        "state_dir": ctx.config.approval.state_dir,
    });
    Ok(json_result(
        &json!({
            "server": {
                "name": env!("CARGO_PKG_NAME"),
                "version": env!("CARGO_PKG_VERSION"),
                "transport": ctx.config.server.transport,
            },
            "policy": ctx.policy.summary(),
            "approval": approvals,
            "moonraker": ctx.config.public_view()["moonraker"].clone(),
            "camera": ctx.config.public_view()["camera"].clone(),
        }),
        None,
    ))
}

async fn pending_approvals(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let records = ctx.approvals.list_open()?;
    let approvals = records
        .iter()
        .map(|record| {
            json!({
                "approval_id": record.id,
                "tool": record.tool,
                "summary": record.summary,
                "risk": record.risk.as_str(),
                "status": record.status,
                "expires_at": record.expires_at,
                "expires_in_secs": record.remaining_secs(),
                "human_approved": record.human_approved_at.is_some(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json_result(
        &json!({
            "count": approvals.len(),
            "approvals": approvals,
            "approval_mode": ctx.config.approval.mode,
        }),
        None,
    ))
}

fn approval_id_schema() -> Value {
    schema_object(
        json!({
            "approval_id": {
                "type": "string",
                "description": "Approval id such as 'apr_...' from a prepare response."
            }
        }),
        &["approval_id"],
    )
}

async fn cancel_approval(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["approval_id"])?;
    let id = args.required_string("approval_id")?;
    let record = ctx.approvals.deny(&id)?;
    Ok(json_result(
        &json!({
            "approval_id": record.id,
            "status": record.status,
            "tool": record.tool,
        }),
        Some("Approval cancelled. The action will not run."),
    ))
}

fn confirm_schema() -> Value {
    schema_object(
        json!({
            "approval_id": {
                "type": "string",
                "description": "Approval id returned by the prepare call."
            },
            "confirmation_phrase": {
                "type": "string",
                "description": "Exact phrase from the approval prompt. Required in in_band and both modes."
            },
            "acknowledge": {
                "type": "boolean",
                "description": "Must be true. Confirms that the action summary was read."
            }
        }),
        &["approval_id", "acknowledge"],
    )
}

async fn confirm_placeholder() -> Result<rust_mcp_sdk::schema::CallToolResult> {
    Err(Error::Approval(
        "confirm_action is handled by the registry and cannot be called directly".to_string(),
    ))
}

/// Ensure the status enum remains serializable for clients.
#[must_use]
pub fn status_label(status: ApprovalStatus) -> &'static str {
    match status {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Approved => "approved",
        ApprovalStatus::Consumed => "consumed",
        ApprovalStatus::Denied => "denied",
        ApprovalStatus::Expired => "expired",
    }
}
