//! MCP tool registry.
//!
//! Every tool is a [`ToolSpec`]: metadata, JSON Schema, a policy classification,
//! and an async handler. The registry is the single choke point for
//! authorization, approval creation, execution, and audit logging, so a new
//! tool cannot accidentally skip the safety model.
//!
//! Three execution gates exist:
//!
//! * [`Gate::Policy`] — evaluated against [`crate::safety::Policy`]. Mutations
//!   may run immediately; `High`/`Critical` actions create a single-use
//!   approval instead.
//! * [`Gate::Emergency`] — always allowed (unless explicitly disabled), never
//!   delayed by an approval prompt.
//! * [`Gate::Internal`] — policy/approval introspection tools that do not touch
//!   the printer.
//!
//! `moonraker_confirm_action` is handled by the registry itself ([`Gate::Confirm`])
//! because it must execute the *stored* tool and arguments, never a fresh set
//! supplied by the caller.

pub mod admin;
pub mod args;
pub mod camera_tools;
pub mod files;
pub mod heaters;
pub mod motion;
pub mod print;
pub mod queue;
pub mod safety_tools;
pub mod state;

use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Instant};

use rust_mcp_sdk::schema::{
    CallToolResult, ContentBlock, ImageContent, TextContent, Tool, ToolAnnotations, ToolInputSchema,
};
use serde_json::{json, Map, Value};

use crate::{
    audit::{AuditLogger, Outcome},
    camera::CameraService,
    config::Config,
    error::{Error, Result},
    moonraker::MoonrakerClient,
    safety::{ApprovalStore, Capability, Decision, Policy, Risk},
};

use args::Args;

/// Boxed future returned by a tool handler.
///
/// The future borrows the tool context, so the lifetime is tied to the `&
/// ToolContext` argument rather than being `'static`.
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<CallToolResult>> + Send + 'a>>;

/// A tool handler function pointer.
pub type ToolFn = for<'a> fn(&'a ToolContext, Value) -> ToolFuture<'a>;

/// Authorization path for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Evaluate the local policy and require approval for high-risk actions.
    Policy,
    /// Safety action; never blocked by read-only mode or approvals.
    Emergency,
    /// Local/introspection tool; no printer-side effect.
    Internal,
    /// Handled specially by the registry (approval confirmation).
    Confirm,
}

/// Static description of one MCP tool.
#[derive(Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub allowed_args: &'static [&'static str],
    pub capability: Capability,
    pub risk: Risk,
    pub gate: Gate,
    /// Human-readable summary shown in the approval prompt.
    pub summarize: fn(&Value) -> String,
    /// JSON Schema for the tool's arguments.
    pub schema: fn() -> Value,
    pub handler: ToolFn,
}

impl ToolSpec {
    fn tool(&self) -> Tool {
        Tool {
            annotations: Some(ToolAnnotations {
                destructive_hint: Some(matches!(self.risk, Risk::High | Risk::Critical)),
                idempotent_hint: Some(false),
                open_world_hint: Some(false),
                read_only_hint: Some(matches!(self.capability, Capability::Read)),
                title: Some(self.title.to_string()),
            }),
            description: Some(self.description.to_string()),
            execution: None,
            icons: Vec::new(),
            input_schema: input_schema(&(self.schema)()),
            meta: None,
            name: self.name.to_string(),
            output_schema: None,
            title: Some(self.title.to_string()),
        }
    }
}

/// Shared services available to every tool.
pub struct ToolContext {
    pub config: Arc<Config>,
    pub client: MoonrakerClient,
    pub camera: CameraService,
    pub policy: Arc<Policy>,
    pub approvals: Arc<ApprovalStore>,
    pub audit: Arc<AuditLogger>,
}

impl ToolContext {
    /// Build the shared context, creating state and audit directories.
    pub fn new(config: Config) -> Result<Self> {
        let client = MoonrakerClient::new(&config.moonraker)?;
        let camera = CameraService::new(client.clone(), config.camera.clone());
        let policy = Policy::new(config.policy.clone());
        let approvals = ApprovalStore::new(
            config.state_dir(),
            config.approval_ttl(),
            config.approval.mode,
        )?;
        let audit_path = config.audit_log_path();
        let audit = AuditLogger::new(audit_path.as_deref())?;
        Ok(Self {
            config: Arc::new(config),
            client,
            camera,
            policy: Arc::new(policy),
            approvals: Arc::new(approvals),
            audit: Arc::new(audit),
        })
    }
}

/// Tool registry.
pub struct ToolRegistry {
    specs: BTreeMap<&'static str, ToolSpec>,
}

impl ToolRegistry {
    /// Build the full registry.
    #[must_use]
    pub fn build() -> Self {
        let mut specs = BTreeMap::new();
        let all = state::specs()
            .into_iter()
            .chain(camera_tools::specs())
            .chain(files::specs())
            .chain(heaters::specs())
            .chain(motion::specs())
            .chain(print::specs())
            .chain(queue::specs())
            .chain(admin::specs())
            .chain(safety_tools::specs());

        for spec in all {
            if let Some(previous) = specs.insert(spec.name, spec) {
                panic!(
                    "duplicate tool name '{}' (was {})",
                    spec.name, previous.name
                );
            }
        }
        Self { specs }
    }

    /// All tools, sorted by name.
    #[must_use]
    pub fn list_tools(&self) -> Vec<Tool> {
        self.specs.values().map(ToolSpec::tool).collect()
    }

    /// Tool names, for `mcp-moonraker tools`.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.specs.keys().copied().collect()
    }

    /// Look up a tool.
    #[must_use]
    pub fn spec(&self, name: &str) -> Option<&ToolSpec> {
        self.specs.get(name)
    }

    /// Call a tool. This is the only public execution entry point.
    pub async fn call(
        &self,
        ctx: &ToolContext,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<CallToolResult> {
        let Some(spec) = self.specs.get(name) else {
            return Err(Error::InvalidArguments(format!("unknown tool '{name}'")));
        };

        if spec.gate == Gate::Confirm {
            return self.confirm(ctx, arguments).await;
        }

        let started = Instant::now();
        let args = Args::parse(arguments, spec.allowed_args)?.into_value();

        match spec.gate {
            Gate::Policy => match ctx.policy.evaluate(spec.capability, spec.risk) {
                Decision::Allow => {}
                Decision::Deny(reason) => {
                    Self::audit_denied(ctx, spec, &args, &reason, started);
                    return Ok(tool_error(&reason));
                }
                Decision::NeedsApproval => {
                    return Self::prepare_approval(ctx, spec, &args, started);
                }
            },
            Gate::Emergency => {
                if !ctx.policy.emergency_stop_allowed() {
                    let reason =
                        "emergency stop is disabled by configuration (disable_emergency_stop=true)";
                    Self::audit_denied(ctx, spec, &args, reason, started);
                    return Ok(tool_error(reason));
                }
            }
            Gate::Internal | Gate::Confirm => {}
        }

        self.execute(ctx, spec, args, started).await
    }

    /// Execute a tool without creating an approval. Used by the confirm path
    /// after the token has been verified. Policy denials still apply, so a
    /// configuration change between prepare and confirm cannot bypass the
    /// new policy.
    pub async fn execute_approved(
        &self,
        ctx: &ToolContext,
        name: &str,
        args: Value,
    ) -> Result<CallToolResult> {
        let Some(spec) = self.specs.get(name) else {
            return Err(Error::Approval(format!(
                "approved tool '{name}' is no longer available"
            )));
        };
        if matches!(spec.gate, Gate::Confirm | Gate::Internal) {
            return Err(Error::Approval(format!(
                "tool '{name}' cannot be executed through an approval"
            )));
        }
        if spec.gate == Gate::Policy {
            if let Decision::Deny(reason) = ctx.policy.evaluate(spec.capability, spec.risk) {
                return Err(Error::Policy(reason));
            }
        }
        if spec.gate == Gate::Emergency && !ctx.policy.emergency_stop_allowed() {
            return Err(Error::Policy(
                "emergency stop is disabled by configuration".to_string(),
            ));
        }
        self.execute(ctx, spec, args, Instant::now()).await
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        spec: &ToolSpec,
        args: Value,
        started: Instant,
    ) -> Result<CallToolResult> {
        let summary_args = args.clone();
        let result = (spec.handler)(ctx, args).await;
        let duration = started.elapsed();
        match &result {
            Ok(value) => {
                let is_error = value.is_error == Some(true);
                ctx.audit.record(
                    spec.name,
                    spec.risk,
                    spec.capability.as_str(),
                    None,
                    &summary_args,
                    if is_error {
                        Outcome::Failed
                    } else {
                        Outcome::Executed
                    },
                    duration,
                    if is_error {
                        Some("tool returned an error result")
                    } else {
                        None
                    },
                );
            }
            Err(err) => {
                ctx.audit.record(
                    spec.name,
                    spec.risk,
                    spec.capability.as_str(),
                    None,
                    &summary_args,
                    Outcome::Failed,
                    duration,
                    Some(&err.public_message()),
                );
            }
        }
        result
    }

    fn prepare_approval(
        ctx: &ToolContext,
        spec: &ToolSpec,
        args: &Value,
        started: Instant,
    ) -> Result<CallToolResult> {
        let summary = (spec.summarize)(args);
        let record = ctx
            .approvals
            .create(spec.name, args.clone(), spec.risk, summary)?;
        ctx.approvals.announce(&record);
        ctx.audit.record(
            spec.name,
            spec.risk,
            spec.capability.as_str(),
            Some(&record.id),
            args,
            Outcome::Prepared,
            started.elapsed(),
            None,
        );
        let prompt = ctx.approvals.prompt(&record);
        Ok(json_result(&prompt, None))
    }

    async fn confirm(&self, ctx: &ToolContext, arguments: Option<Value>) -> Result<CallToolResult> {
        let started = Instant::now();
        let args = Args::parse(
            arguments,
            &["approval_id", "confirmation_phrase", "acknowledge"],
        )?;
        let id = args.required_string("approval_id")?;
        let phrase = args.optional_string("confirmation_phrase")?;
        let acknowledge = args.required_bool("acknowledge")?;

        let record = match ctx.approvals.consume(&id, phrase.as_deref(), acknowledge) {
            Ok(record) => record,
            Err(err) => {
                ctx.audit.record(
                    "moonraker_confirm_action",
                    Risk::High,
                    "mutation",
                    Some(&id),
                    &args.as_value(),
                    Outcome::Denied,
                    started.elapsed(),
                    Some(&err.public_message()),
                );
                return Ok(tool_error(&format!(
                    "{}\n\nNo action was executed.",
                    err.public_message()
                )));
            }
        };

        ctx.audit.record(
            "moonraker_confirm_action",
            record.risk,
            "mutation",
            Some(&record.id),
            &json!({"tool": record.tool, "summary": record.summary}),
            Outcome::Confirmed,
            started.elapsed(),
            None,
        );

        let result = self
            .execute_approved(ctx, &record.tool, record.args.clone())
            .await;

        let note = match &result {
            Ok(value) if value.is_error == Some(true) => {
                let message = first_text(value).unwrap_or_else(|| "tool error".to_string());
                let note = format!(
                    "Approval {} was consumed, but '{}' returned an error. It cannot be retried; \
                     prepare the action again if needed.\n{}",
                    record.id, record.tool, message
                );
                let _ = ctx.approvals.finish(&record.id, &note);
                return Ok(tool_error(&note));
            }
            Ok(_) => format!(
                "Approval {} consumed: '{}' executed ({})",
                record.id, record.tool, record.summary
            ),
            Err(err) => format!(
                "Approval {} was consumed, but '{}' failed: {}. It cannot be retried; prepare the \
                 action again if needed.",
                record.id,
                record.tool,
                err.public_message()
            ),
        };

        let _ = ctx.approvals.finish(&record.id, &note);
        match result {
            Ok(value) => Ok(prepend_text(value, &note)),
            Err(_err) => Ok(tool_error(&note)),
        }
    }

    fn audit_denied(
        ctx: &ToolContext,
        spec: &ToolSpec,
        args: &Value,
        reason: &str,
        started: Instant,
    ) {
        ctx.audit.record(
            spec.name,
            spec.risk,
            spec.capability.as_str(),
            None,
            args,
            Outcome::Denied,
            started.elapsed(),
            Some(reason),
        );
    }
}

/// Convert a JSON Schema object into the SDK's input-schema type.
fn input_schema(schema: &Value) -> ToolInputSchema {
    let object = schema
        .as_object()
        .expect("tool schemas must be JSON objects");
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(key, value)| {
                    let value = value
                        .as_object()
                        .unwrap_or_else(|| panic!("schema for '{key}' must be an object"))
                        .clone();
                    (key.clone(), value)
                })
                .collect::<BTreeMap<String, Map<String, Value>>>()
        });
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ToolInputSchema::new(required, properties, None)
}

/// Text result from a JSON value.
#[must_use]
pub fn json_result(value: &impl serde::Serialize, preface: Option<&str>) -> CallToolResult {
    let body = serde_json::to_string_pretty(value)
        .unwrap_or_else(|err| format!("{{\"error\":\"could not serialize result: {err}\"}}"));
    let text = match preface {
        Some(preface) => format!("{preface}\n\n{body}"),
        None => body,
    };
    CallToolResult::text_content(vec![TextContent::from(text)])
}

/// Plain text result.
#[must_use]
pub fn text_result(text: impl Into<String>) -> CallToolResult {
    CallToolResult::text_content(vec![TextContent::from(text.into())])
}

/// Error result that the model can read and correct.
#[must_use]
pub fn tool_error(message: &str) -> CallToolResult {
    CallToolResult {
        content: vec![TextContent::from(message.to_string()).into()],
        is_error: Some(true),
        meta: None,
        structured_content: None,
    }
}

/// Image result.
#[must_use]
pub fn image_result(data: String, mime_type: String) -> CallToolResult {
    CallToolResult::image_content(vec![ImageContent::new(data, mime_type, None, None)])
}

/// Prepend a text block to an existing result.
#[must_use]
pub fn prepend_text(mut result: CallToolResult, text: &str) -> CallToolResult {
    if result
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::TextContent(_)))
    {
        result.content.insert(
            0,
            ContentBlock::TextContent(TextContent::from(text.to_string())),
        );
    } else {
        result
            .content
            .push(ContentBlock::TextContent(TextContent::from(
                text.to_string(),
            )));
    }
    result
}

fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        ContentBlock::TextContent(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// Default summary for tools that never need approval.
#[must_use]
pub fn no_summary(_args: &Value) -> String {
    String::new()
}

/// Build a JSON Schema object with `additionalProperties: false`.
///
/// `properties` is taken by value so the schema builders can pass `json!`
/// literals directly. `json!` borrows it internally, which Clippy flags even
/// though moving it is the clearer API.
#[allow(clippy::needless_pass_by_value)]
#[must_use]
pub fn schema_object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Schema for a tool with no arguments.
#[must_use]
pub fn empty_schema() -> Value {
    schema_object(json!({}), &[])
}

/// Common object-query helper used by several tools.
pub async fn query_objects(ctx: &ToolContext, objects: Map<String, Value>) -> Result<Value> {
    ctx.client
        .call_value("printer.objects.query", Some(json!({"objects": objects})))
        .await
}

/// Fetch `printer.objects.list`.
pub async fn object_names(ctx: &ToolContext) -> Result<Vec<String>> {
    let value = ctx.client.call_value("printer.objects.list", None).await?;
    Ok(value
        .get("objects")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// Ensure an object exists before using it in a G-code command.
pub async fn require_object(ctx: &ToolContext, name: &str, what: &str) -> Result<()> {
    let names = object_names(ctx).await?;
    if names.iter().any(|candidate| candidate == name) {
        Ok(())
    } else {
        Err(Error::InvalidArguments(format!(
            "{what} '{name}' is not a loaded Klipper object"
        )))
    }
}

/// Validate a JSON Schema object's required list for tests.
#[cfg(test)]
pub fn schema_properties(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}
