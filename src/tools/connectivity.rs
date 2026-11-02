//! Connectivity verification tool.
//!
//! [`moonraker_verify_connection`] is a deliberate, always-safe first call: it
//! probes Moonraker with `server.info`, measures round-trip latency, and
//! **returns a structured verdict instead of an error** when Moonraker is
//! unreachable or rejects the credentials. Agents (and humans reading the
//! transcript) can distinguish "wrong URL" from "missing API key" without
//! tripping other tools into unexpected behaviour, and can refuse to chain
//! further calls until `connected` is `true`.

use serde_json::json;

use crate::{
    error::Result,
    moonraker::types::ServerInfo,
    safety::redact::redact_url_userinfo,
    safety::{Capability, Risk},
    tools::{empty_schema, json_result, no_summary, Gate, ToolContext, ToolSpec},
};
use rust_mcp_sdk::schema::CallToolResult;

/// Connectivity tool specification.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "moonraker_verify_connection",
        title: "Verify Moonraker connection",
        description: "Probe Moonraker and report whether this MCP server can reach it: \
                      connected flag, round-trip latency, Moonraker/Klipper versions, and — on \
                      failure — a classified diagnosis with next steps. Always call this first \
                      (or before retrying) when another moonraker_* tool fails with a transport \
                      error or an unauthorized error. Read-only, never blocks, never mutates.",
        allowed_args: &[],
        capability: Capability::Read,
        risk: Risk::Read,
        gate: Gate::Policy,
        summarize: no_summary,
        schema: empty_schema,
        handler: |ctx, _args| Box::pin(verify_connection(ctx)),
    }]
}

async fn verify_connection(ctx: &ToolContext) -> Result<CallToolResult> {
    let started = std::time::Instant::now();
    match ctx.client.call_value("server.info", None).await {
        Ok(value) => {
            let latency = started.elapsed().as_millis();
            let info: ServerInfo = serde_json::from_value(value).unwrap_or_default();
            let verdict = if info.klippy_connected && info.klippy_state == "ready" {
                "healthy: Moonraker is reachable and Klipper is ready; moonraker_* tools can be \
                 called normally"
            } else if info.klippy_connected {
                "reachable: Moonraker is reachable but Klippy is not ready yet (state below); \
                 print and motion tools may fail until Klipper reaches ready"
            } else {
                "reachable: Moonraker is reachable but Klippy is disconnected; printer-state \
                 tools will return stale or empty data until it reconnects"
            };
            Ok(json_result(
                &json!({
                    "connected": true,
                    "latency_ms": latency,
                    "url": redact_url_userinfo(&ctx.config.moonraker.url),
                    "api_key_configured": ctx.client.has_api_key(),
                    "moonraker_version": info.moonraker_version,
                    "api_version": info.api_version_string,
                    "klippy_connected": info.klippy_connected,
                    "klippy_state": info.klippy_state,
                    "failed_components": info.failed_components,
                    "warnings": info.warnings,
                    "verdict": verdict,
                }),
                None,
            ))
        }
        Err(err) => {
            let latency = started.elapsed().as_millis();
            Ok(json_result(
                &json!({
                    "connected": false,
                    "latency_ms": latency,
                    "url": redact_url_userinfo(&ctx.config.moonraker.url),
                    "api_key_configured": ctx.client.has_api_key(),
                    "diagnosis": err.public_message(),
                    "next_steps": verify_next_steps(),
                }),
                None,
            ))
        }
    }
}

/// Concrete actions for the agent/human, ordered by likelihood.
fn verify_next_steps() -> Vec<&'static str> {
    vec![
        "stop calling other moonraker_* tools until connected is true; they will fail with \
         the same diagnosis",
        "check MOONRAKER_URL: the default http://127.0.0.1:7125 only works when this server \
         runs on the same host as Moonraker; 127.0.0.1 from a container means the container",
        "run 'mcp-moonraker doctor' (with the same --config/-environment) on the machine \
         running this server to reproduce the failure outside the MCP client",
        "if the diagnosis mentions 401: fetch the API key with \
         'curl http://127.0.0.1:7125/access/api_key' on the printer host and set \
         MOONRAKER_API_KEY (see docs/setup-and-security.md)",
    ]
}
