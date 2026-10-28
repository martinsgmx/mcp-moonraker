//! Host administration tools.
//!
//! Disabled by default (`MCP_MOONRAKER_ALLOW_HOST_ADMIN=false`). When enabled,
//! every action still requires double confirmation. These tools affect the
//! machine running Klipper, not just the printer, so they are `Critical` risk.

use serde_json::{json, Value};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{args::Args, json_result, no_summary, schema_object, Gate, ToolContext, ToolSpec},
};

/// Host administration tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_update_status",
            title: "Software update status",
            description: "Return Moonraker's update manager status for Klipper, Moonraker, and \
                          other configured components, including available versions and dirty \
                          states. Read-only.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(update_status(ctx)),
        },
        ToolSpec {
            name: "moonraker_update_refresh",
            title: "Refresh update information",
            description: "ADMIN: ask Moonraker's update manager to refresh version information. \
                          Requires host administration to be enabled and double confirmation.",
            allowed_args: &["name"],
            capability: Capability::HostAdmin,
            risk: Risk::Critical,
            gate: Gate::Policy,
            summarize: summarize_refresh,
            schema: || name_schema("Component to refresh, for example 'klipper' or 'moonraker'."),
            handler: |ctx, args| Box::pin(update_refresh(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_update_upgrade",
            title: "Upgrade a software component",
            description: "ADMIN: upgrade one named component through Moonraker's update manager. \
                          This can restart services and may require a Klipper restart. Requires \
                          host administration and double confirmation. A component name is \
                          mandatory to avoid accidental blanket upgrades.",
            allowed_args: &["name"],
            capability: Capability::HostAdmin,
            risk: Risk::Critical,
            gate: Gate::Policy,
            summarize: summarize_upgrade,
            schema: || name_schema("Component to upgrade, for example 'klipper' or 'moonraker'."),
            handler: |ctx, args| Box::pin(update_upgrade(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_service_restart",
            title: "Restart a host service",
            description: "ADMIN: restart a system service managed by Moonraker (for example \
                          'klipper' or 'moonraker'). Requires host administration and double \
                          confirmation.",
            allowed_args: &["service"],
            capability: Capability::HostAdmin,
            risk: Risk::Critical,
            gate: Gate::Policy,
            summarize: summarize_service,
            schema: service_schema,
            handler: |ctx, args| Box::pin(service_restart(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_host_reboot",
            title: "Reboot the printer host",
            description:
                "ADMIN: reboot the machine running Moonraker/Klipper. The printer will be \
                          unavailable while it boots. Requires host administration and double \
                          confirmation.",
            allowed_args: &[],
            capability: Capability::HostAdmin,
            risk: Risk::Critical,
            gate: Gate::Policy,
            summarize: |_| "Reboot the printer host machine.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(host_reboot(ctx)),
        },
        ToolSpec {
            name: "moonraker_host_shutdown",
            title: "Shut down the printer host",
            description: "ADMIN: shut down the machine running Moonraker/Klipper. It will need \
                          manual power-on or Wake-on-LAN. Requires host administration and double \
                          confirmation.",
            allowed_args: &[],
            capability: Capability::HostAdmin,
            risk: Risk::Critical,
            gate: Gate::Policy,
            summarize: |_| {
                "Shut down the printer host machine. Manual power-on will be required.".to_string()
            },
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(host_shutdown(ctx)),
        },
    ]
}

async fn update_status(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let value = ctx.client.call_value("machine.update.status", None).await?;
    Ok(json_result(
        &crate::safety::redact::redact_value(&value),
        None,
    ))
}

fn name_schema(description: &str) -> Value {
    schema_object(
        json!({
            "name": {
                "type": "string",
                "description": description
            }
        }),
        &["name"],
    )
}

fn validate_component(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(Error::InvalidArguments(
            "component name contains unsupported characters".to_string(),
        ));
    }
    Ok(name.to_string())
}

fn summarize_refresh(args: &Value) -> String {
    format!(
        "Refresh update information for component '{}'.",
        args.get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
    )
}

async fn update_refresh(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["name"])?;
    let name = validate_component(&args.required_string("name")?)?;
    let value = ctx
        .client
        .call_value("machine.update.refresh", Some(json!({"name": name})))
        .await?;
    Ok(json_result(&value, None))
}

fn summarize_upgrade(args: &Value) -> String {
    format!(
        "Upgrade software component '{}'. Services may restart and an active print may fail.",
        args.get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
    )
}

async fn update_upgrade(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["name"])?;
    let name = validate_component(&args.required_string("name")?)?;
    let value = ctx
        .client
        .call_value("machine.update.upgrade", Some(json!({"name": name})))
        .await?;
    Ok(json_result(&value, None))
}

fn service_schema() -> Value {
    schema_object(
        json!({
            "service": {
                "type": "string",
                "description": "Service name from Moonraker's available_services list, for example 'klipper' or 'moonraker'."
            }
        }),
        &["service"],
    )
}

fn summarize_service(args: &Value) -> String {
    format!(
        "Restart host service '{}'. The printer may disconnect or an active print may fail.",
        args.get("service")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
    )
}

async fn service_restart(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["service"])?;
    let service = validate_component(&args.required_string("service")?)?;
    let value = ctx
        .client
        .call_value(
            "machine.services.restart",
            Some(json!({"service": service})),
        )
        .await?;
    Ok(json_result(&value, None))
}

async fn host_reboot(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let value = ctx.client.call_value("machine.reboot", None).await?;
    Ok(json_result(
        &json!({"result": value}),
        Some("Host reboot requested. The MCP server will lose its connection."),
    ))
}

async fn host_shutdown(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let value = ctx.client.call_value("machine.shutdown", None).await?;
    Ok(json_result(
        &json!({"result": value}),
        Some("Host shutdown requested. Manual power-on will be required."),
    ))
}
