//! MCP server handler: tools, resources, and prompts.
//!
//! The handler is intentionally thin. All authorization, approval, and audit
//! behavior lives in [`crate::tools::ToolRegistry`], so resources and prompts
//! cannot bypass it either.

use std::sync::Arc;

use async_trait::async_trait;
use rust_mcp_sdk::{
    mcp_server::ServerHandler,
    schema::{
        schema_utils::CallToolError, CallToolRequestParams, CallToolResult, ContentBlock,
        GetPromptRequestParams, GetPromptResult, Implementation, InitializeResult,
        ListPromptsResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams, Prompt,
        PromptArgument, PromptMessage, ProtocolVersion, ReadResourceRequestParams,
        ReadResourceResult, Role, RpcError, ServerCapabilities, ServerCapabilitiesPrompts,
        ServerCapabilitiesResources, ServerCapabilitiesTools, TextContent, TextResourceContents,
    },
    McpServer,
};
use serde_json::{Map, Value};

use crate::{
    error::Error,
    tools::{text_result, tool_error, ToolContext, ToolRegistry},
};

/// Build the MCP `InitializeResult` advertised to clients.
#[must_use]
pub fn server_details() -> InitializeResult {
    InitializeResult {
        server_info: Implementation {
            name: "mcp-moonraker".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: Some("mcp-moonraker".into()),
            description: Some(
                "Safe, structured access to Moonraker/Klipper: state, cameras, files, motion, \
                 temperatures, and approval-gated destructive actions."
                    .into(),
            ),
            icons: Vec::new(),
            website_url: Some("https://github.com/martinsgmx/mcp-moonraker".into()),
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools {
                list_changed: Some(false),
            }),
            resources: Some(ServerCapabilitiesResources {
                list_changed: Some(false),
                subscribe: Some(false),
            }),
            prompts: Some(ServerCapabilitiesPrompts {
                list_changed: Some(false),
            }),
            completions: Some(Map::new()),
            ..Default::default()
        },
        meta: None,
        instructions: Some(instructions()),
        protocol_version: ProtocolVersion::V2025_11_25.into(),
    }
}

fn instructions() -> String {
    [
        "You are connected to a 3D printer through Moonraker.",
        "",
        "Connectivity: if any moonraker_* tool fails with a transport, timeout, or \
         unauthorized (401) error, call moonraker_verify_connection next. It reports \
         connected, latency, versions, and a classified diagnosis; do not call other \
         printer tools until it reports connected=true.",
        "",
        "Reading state is always allowed: use moonraker_printer_status for an overview, \
         moonraker_temperatures for heaters/sensors, and moonraker_camera_snapshot for a \
         current frame.",
        "",
        "Mutation rules:",
        "- Low/medium-risk tools (heaters, fans, XY jogs, speed/flow, pause/resume, queue \
         additions) run immediately when mutations are enabled.",
        "- High/critical-risk tools return an approval prompt instead of acting. To execute, \
         call moonraker_confirm_action with the approval_id, the confirmation_phrase from the \
         prompt (when shown), and acknowledge=true. The phrase is single-use and expires.",
        "- In out-of-band approval mode a human must also run `mcp-moonraker approve <id>` on \
         the server host before confirmation succeeds.",
        "- moonraker_emergency_stop is immediate and never waits for approval. Use it only for \
         a real physical emergency.",
        "",
        "Never fabricate temperatures or print state: every claim should come from a tool result. \
         If a tool fails, report the error rather than guessing.",
    ]
    .join("\n")
}

/// MCP server handler.
pub struct MoonrakerServerHandler {
    registry: Arc<ToolRegistry>,
    context: Arc<ToolContext>,
}

impl MoonrakerServerHandler {
    /// Create a handler around a registry and its shared context.
    #[must_use]
    pub fn new(registry: Arc<ToolRegistry>, context: Arc<ToolContext>) -> Self {
        Self { registry, context }
    }

    async fn resource_text(&self, tool: &str) -> Result<String, RpcError> {
        let result = self
            .registry
            .call(&self.context, tool, None)
            .await
            .map_err(|err| RpcError::internal_error().with_message(err.public_message()))?;
        if result.is_error == Some(true) {
            let message = result
                .content
                .iter()
                .find_map(|block| match block {
                    ContentBlock::TextContent(text) => Some(text.text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "resource read failed".to_string());
            return Err(RpcError::internal_error().with_message(message));
        }
        let text = result
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::TextContent(text) => Some(text.text.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                RpcError::internal_error().with_message("resource returned no text content")
            })?;
        Ok(text)
    }
}

#[async_trait]
impl ServerHandler for MoonrakerServerHandler {
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: self.registry.list_tools(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        if self.registry.spec(&params.name).is_none() {
            return Err(CallToolError::unknown_tool(params.name));
        }
        let arguments = params.arguments.map(Value::Object);
        match self
            .registry
            .call(&self.context, &params.name, arguments)
            .await
        {
            Ok(result) => Ok(result),
            Err(err) => Ok(tool_error(&err.public_message())),
        }
    }

    async fn handle_list_resources_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListResourcesResult, RpcError> {
        Ok(ListResourcesResult {
            resources: resource_catalog(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_read_resource_request(
        &self,
        params: ReadResourceRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ReadResourceResult, RpcError> {
        let tool = match params.uri.as_str() {
            "moonraker://status" => "moonraker_printer_status",
            "moonraker://temperatures" => "moonraker_temperatures",
            "moonraker://endstops" => "moonraker_endstops",
            "moonraker://cameras" => "moonraker_cameras",
            "moonraker://config" => "moonraker_config",
            "moonraker://system" => "moonraker_system_info",
            "moonraker://policy" => "moonraker_policy_info",
            "moonraker://approvals" => "moonraker_pending_approvals",
            other => {
                return Err(RpcError::invalid_params()
                    .with_message(format!("unknown resource URI '{other}'")));
            }
        };

        let text = self.resource_text(tool).await?;
        Ok(ReadResourceResult {
            contents: vec![TextResourceContents::new(text, params.uri)
                .with_mime_type("application/json")
                .into()],
            meta: None,
        })
    }

    async fn handle_list_prompts_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListPromptsResult, RpcError> {
        Ok(ListPromptsResult {
            prompts: prompt_catalog(),
            meta: None,
            next_cursor: None,
        })
    }

    async fn handle_get_prompt_request(
        &self,
        params: GetPromptRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<GetPromptResult, RpcError> {
        let arguments = params.arguments.unwrap_or_default();
        let (description, text) = match params.name.as_str() {
            "printer_health_check" => (
                "Check printer health and summarize any problems.",
                "Check the connected printer's health. Use moonraker_server_info, \
                 moonraker_printer_status, moonraker_temperatures, and moonraker_endstops. \
                 Report Klipper state, current print state/progress, heater actual-vs-target \
                 temperatures, homing state, and any warnings. Do not change anything; if a \
                 problem needs action, explain the exact tool and arguments you would use."
                    .to_string(),
            ),
            "print_failure_triage" => {
                let symptom = arguments
                    .get("symptom")
                    .map_or("the print failed", String::as_str)
                    .to_string();
                (
                    "Triage a print failure using read-only diagnostics.",
                    format!(
                        "A print failure was reported: {symptom}. Gather evidence with \
                         moonraker_printer_status, moonraker_temperatures, moonraker_endstops, \
                         moonraker_gcode_history, and moonraker_print_history. Identify the most \
                         likely cause and rank next diagnostic steps. Do not run destructive tools; \
                         propose them with their approval implications if needed."
                    ),
                )
            }
            other => {
                return Err(
                    RpcError::invalid_params().with_message(format!("unknown prompt '{other}'"))
                );
            }
        };

        Ok(GetPromptResult {
            description: Some(description.to_string()),
            messages: vec![PromptMessage {
                role: Role::User,
                content: ContentBlock::TextContent(TextContent::from(text)),
            }],
            meta: None,
        })
    }
}

fn resource_catalog() -> Vec<rust_mcp_sdk::schema::Resource> {
    [
        (
            "moonraker://status",
            "Printer status",
            "Curated printer and print-job state snapshot.",
        ),
        (
            "moonraker://temperatures",
            "Temperatures",
            "Current and target temperatures for heaters and sensors.",
        ),
        (
            "moonraker://endstops",
            "Endstops",
            "Current endstop trigger states.",
        ),
        (
            "moonraker://cameras",
            "Cameras",
            "Configured webcams and snapshot availability.",
        ),
        (
            "moonraker://config",
            "Configuration",
            "Redacted Moonraker configuration.",
        ),
        (
            "moonraker://system",
            "System information",
            "Host system and Moonraker process statistics.",
        ),
        (
            "moonraker://policy",
            "Safety policy",
            "Local mutation policy and approval mode.",
        ),
        (
            "moonraker://approvals",
            "Pending approvals",
            "Destructive actions waiting for confirmation.",
        ),
    ]
    .into_iter()
    .map(|(uri, name, description)| rust_mcp_sdk::schema::Resource {
        uri: uri.to_string(),
        name: name.to_string(),
        title: Some(name.to_string()),
        description: Some(description.to_string()),
        mime_type: Some("application/json".to_string()),
        size: None,
        annotations: None,
        icons: Vec::new(),
        meta: None,
    })
    .collect()
}

fn prompt_catalog() -> Vec<Prompt> {
    vec![
        Prompt {
            name: "printer_health_check".to_string(),
            title: Some("Printer health check".to_string()),
            description: Some(
                "Read-only health check covering state, temperatures, homing, and warnings."
                    .to_string(),
            ),
            arguments: Vec::new(),
            icons: Vec::new(),
            meta: None,
        },
        Prompt {
            name: "print_failure_triage".to_string(),
            title: Some("Print failure triage".to_string()),
            description: Some(
                "Read-only diagnostic workflow for a failed or failing print.".to_string(),
            ),
            arguments: vec![PromptArgument {
                name: "symptom".to_string(),
                title: Some("Symptom".to_string()),
                description: Some("What went wrong, for example 'layer shift at 40%'.".to_string()),
                required: Some(false),
            }],
            icons: Vec::new(),
            meta: None,
        },
    ]
}

/// Serialize a JSON value into a text tool result; used by tests.
#[must_use]
pub fn json_text(value: &Value) -> CallToolResult {
    text_result(serde_json::to_string_pretty(value).unwrap_or_default())
}

/// Convenience conversion used by doctor output.
#[must_use]
pub fn error_text(err: &Error) -> String {
    err.public_message()
}
