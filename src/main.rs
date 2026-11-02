//! Command-line interface for the mcp-moonraker server.
//!
//! Running the binary with no subcommand starts the stdio server, so an MCP
//! client can be configured with just the executable path. Operational
//! subcommands cover the pieces that happen outside an MCP session: probing a
//! Moonraker instance, generating a starter config, and approving or denying
//! destructive actions when out-of-band approval is enabled.
//!
//! stdout is reserved for MCP messages in server mode and for command output in
//! every other mode. Diagnostics and tracing always go to stderr.

use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Args, Parser, Subcommand};
use mcp_moonraker::{
    config::{Config, TransportKind},
    error::{Error, Result},
    moonraker::types::{PrinterInfo, ServerInfo},
    safety::{approval::ApprovalStore, Decision, Policy},
    server::{server_details, MoonrakerServerHandler},
    tools::{Gate, ToolContext, ToolRegistry},
    transport,
};

/// Starter configuration written by `mcp-moonraker init`.
///
/// It deliberately contains no secrets. API keys and bearer tokens belong in
/// environment variables or a secret manager.
const STARTER_CONFIG: &str = r#"# mcp-moonraker configuration
#
# Precedence: built-in defaults < this file < environment variables < CLI flags.
# Keep secrets out of this file. Use MOONRAKER_API_KEY, or
# MCP_MOONRAKER_HTTP_TOKEN for the HTTP transport.

[moonraker]
url = "http://127.0.0.1:7125"
timeout_secs = 15
# printer.gcode.script only replies once Klipper has finished the script, so
# homing (G28) and long macros use this larger budget.
script_timeout_secs = 120
tls_verify = true
max_response_bytes = 8388608

[server]
# "stdio" is the zero-friction default for local agents.
transport = "stdio"

[server.http]
host = "127.0.0.1"
port = 8765
path = "/mcp"
allow_unauthenticated = false

[policy]
read_only = false
allow_mutations = true
# Arbitrary G-code is powerful and easy to misuse. Leave this off unless needed.
allow_raw_gcode = false
# Service restarts, reboots, shutdowns, and updates.
allow_host_admin = false
allow_config_write = false
max_hotend_temp = 300.0
max_bed_temp = 120.0
disable_emergency_stop = false

[approval]
# in_band: the agent receives a single-use phrase.
# out_of_band: a human must run `mcp-moonraker approve <id>`.
# both: phrase plus human approval (recommended for remote deployments).
mode = "in_band"
ttl_secs = 120

[camera]
max_bytes = 8388608
timeout_secs = 10
allow_private_hosts = true

[logging]
level = "info"
"#;

#[derive(Debug, Parser)]
#[command(
    name = "mcp-moonraker",
    version,
    about = "Safe, structured MCP server for Moonraker / Klipper",
    long_about = "A Model Context Protocol server for Moonraker and Klipper. It exposes \
                  read-only state, camera frames, temperatures, controlled settings changes, and \
                  double-confirmed destructive actions.\n\n\
                  Running without a subcommand starts the stdio server, which is what most MCP \
                  clients need.",
    propagate_version = true
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, global = true, env = "MCP_MOONRAKER_CONFIG", value_name = "FILE")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the MCP server (default when no subcommand is given).
    Serve(ServeArgs),
    /// Check configuration, Moonraker connectivity, Klipper state, and cameras.
    Doctor(DoctorArgs),
    /// Mark a destructive action as human-approved (out-of-band approval).
    Approve {
        /// Approval id from the prepare response, for example `apr_0123...`.
        approval_id: String,
    },
    /// Cancel a destructive action so it can never run.
    Deny {
        /// Approval id from the prepare response.
        approval_id: String,
    },
    /// List prepared destructive actions.
    Pending(PendingArgs),
    /// Print the fully resolved, secret-free configuration.
    PrintConfig(PrintConfigArgs),
    /// List the MCP tools, their risk, and their authorization path.
    Tools(ToolsArgs),
    /// Write a starter configuration file.
    Init(InitArgs),
}

#[derive(Debug, Args, Default)]
struct ServeArgs {
    /// Transport: stdio (default) or http.
    #[arg(long, value_name = "TRANSPORT")]
    transport: Option<String>,

    /// Moonraker base URL, for example `http://printer.local:7125`.
    #[arg(long, value_name = "URL")]
    url: Option<String>,

    /// HTTP bind host (only used with --transport http).
    #[arg(long, value_name = "HOST")]
    http_host: Option<String>,

    /// HTTP bind port (only used with --transport http).
    #[arg(long, value_name = "PORT")]
    http_port: Option<u16>,

    /// Disable every mutation except emergency stop.
    #[arg(long)]
    read_only: bool,

    /// Enable the arbitrary G-code tool (still double-confirmed).
    #[arg(long)]
    allow_raw_gcode: bool,

    /// Enable host administration tools (still double-confirmed).
    #[arg(long)]
    allow_host_admin: bool,

    /// Approval model: in-band, out-of-band, or both.
    #[arg(long, value_name = "MODE")]
    approval_mode: Option<String>,

    /// Directory for approval state.
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,

    /// Tracing filter, for example `info` or `mcp_moonraker=debug`.
    #[arg(long, value_name = "FILTER")]
    log_level: Option<String>,
}

impl ServeArgs {
    fn apply(&self, config: &mut Config) -> Result<()> {
        if let Some(transport) = &self.transport {
            config.server.transport = transport.parse().map_err(Error::Config)?;
        }
        if let Some(url) = &self.url {
            config.moonraker.url.clone_from(url);
        }
        if let Some(host) = &self.http_host {
            config.server.http.host.clone_from(host);
        }
        if let Some(port) = self.http_port {
            config.server.http.port = port;
        }
        if self.read_only {
            config.policy.read_only = true;
        }
        if self.allow_raw_gcode {
            config.policy.allow_raw_gcode = true;
        }
        if self.allow_host_admin {
            config.policy.allow_host_admin = true;
        }
        if let Some(mode) = &self.approval_mode {
            config.approval.mode = mode.parse().map_err(Error::Config)?;
        }
        if let Some(dir) = &self.state_dir {
            config.approval.state_dir = dir.to_string_lossy().to_string();
        }
        if let Some(level) = &self.log_level {
            config.logging.level.clone_from(level);
        }
        config.revalidate()
    }
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Emit a machine-readable JSON report.
    #[arg(long)]
    json: bool,

    /// Exit non-zero when there are warnings, not only failures.
    #[arg(long)]
    strict: bool,

    /// Override the configured Moonraker URL for this check only.
    #[arg(long, value_name = "URL")]
    url: Option<String>,
}

#[derive(Debug, Args)]
struct PendingArgs {
    /// Include consumed, denied, and expired records.
    #[arg(long)]
    all: bool,

    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct PrintConfigArgs {
    /// Emit JSON instead of TOML.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ToolsArgs {
    /// Emit JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Destination file, or '-' for stdout.
    #[arg(long, short, value_name = "FILE", default_value = "mcp-moonraker.toml")]
    output: String,

    /// Overwrite an existing file.
    #[arg(long)]
    force: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("mcp-moonraker: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        // `tools` and `init` do not need a valid runtime configuration.
        Some(Command::Tools(args)) => Ok(list_tools(&args)),
        Some(Command::Init(args)) => init_config(&args),
        Some(command) => run_with_config(cli.config.as_deref(), command).await,
        None => run_with_config(cli.config.as_deref(), Command::Serve(ServeArgs::default())).await,
    }
}

async fn run_with_config(config_path: Option<&Path>, command: Command) -> Result<ExitCode> {
    let mut config = Config::load(config_path)?;

    match command {
        Command::Serve(args) => {
            args.apply(&mut config)?;
            init_tracing(&config.logging.level);
            serve(&config).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor(args) => {
            if let Some(url) = &args.url {
                config.moonraker.url = url.clone();
                config.revalidate()?;
            }
            init_tracing(&config.logging.level);
            doctor(&config, &args).await
        }
        Command::Approve { approval_id } => {
            init_tracing(&config.logging.level);
            approve(&config, &approval_id)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Deny { approval_id } => {
            init_tracing(&config.logging.level);
            deny(&config, &approval_id)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Pending(args) => {
            init_tracing(&config.logging.level);
            pending(&config, &args)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::PrintConfig(args) => print_config(&config, &args),
        Command::Tools(_) | Command::Init(_) => unreachable!("handled before config load"),
    }
}

/// Initialise tracing with stderr as the only sink.
///
/// This is essential: stdout is the MCP protocol channel in stdio mode. A
/// stray log line on stdout would corrupt the session.
fn init_tracing(level: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

async fn serve(config: &Config) -> Result<()> {
    let context = Arc::new(ToolContext::new(config.clone())?);
    let registry = Arc::new(ToolRegistry::build());
    let handler = MoonrakerServerHandler::new(registry, context);

    tracing::info!(
        target: "mcp_moonraker",
        version = env!("CARGO_PKG_VERSION"),
        transport = ?config.server.transport,
        moonraker = %mcp_moonraker::safety::redact::redact_url_userinfo(&config.moonraker.url),
        "starting mcp-moonraker server"
    );

    let details = server_details();
    match config.server.transport {
        TransportKind::Stdio => transport::run_stdio(config, handler, details).await,
        TransportKind::Http => transport::run_http(config, handler, details).await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug)]
struct Check {
    name: &'static str,
    status: CheckStatus,
    detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Ok,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Warn,
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Fail,
            detail: detail.into(),
        }
    }
}

async fn doctor(config: &Config, args: &DoctorArgs) -> Result<ExitCode> {
    let mut checks = Vec::new();

    // Constructing the full tool context also proves that the state directory
    // and audit log location are usable before an agent tries to use them.
    let context = match ToolContext::new(config.clone()) {
        Ok(context) => Some(Arc::new(context)),
        Err(err) => {
            checks.push(Check::fail(
                "local state",
                format!(
                    "cannot initialise state directory: {}",
                    err.public_message()
                ),
            ));
            None
        }
    };

    checks.push(Check::ok(
        "configuration",
        format!(
            "transport={:?}, approval={:?}, read_only={}, mutations={}",
            config.server.transport,
            config.approval.mode,
            config.policy.read_only,
            config.policy.allow_mutations
        ),
    ));

    let mut server_info = None;
    if let Some(context) = &context {
        match context.client.call::<ServerInfo>("server.info", None).await {
            Ok(info) => {
                checks.push(Check::ok(
                    "Moonraker",
                    format!(
                        "reachable at {} (Moonraker {}, API {})",
                        mcp_moonraker::safety::redact::redact_url_userinfo(&config.moonraker.url),
                        non_empty(&info.moonraker_version, "unknown"),
                        non_empty(&info.api_version_string, "unknown")
                    ),
                ));
                if info.failed_components.is_empty() {
                    checks.push(Check::ok("components", "no failed Moonraker components"));
                } else {
                    checks.push(Check::fail(
                        "components",
                        format!("failed components: {}", info.failed_components.join(", ")),
                    ));
                }
                for warning in &info.warnings {
                    checks.push(Check::warn("Moonraker warning", warning.clone()));
                }
                server_info = Some(info);
            }
            Err(err) => {
                checks.push(Check::fail(
                    "Moonraker",
                    format!("unreachable: {}", err.public_message()),
                ));
            }
        }
    }

    let mut printer_reachable = false;
    if let Some(context) = &context {
        if server_info.is_some() {
            match context
                .client
                .call::<PrinterInfo>("printer.info", None)
                .await
            {
                Ok(printer) => {
                    printer_reachable = true;
                    let state = non_empty(&printer.state, "unknown");
                    let message = non_empty(&printer.state_message, "");
                    let status = match state {
                        "ready" | "printing" | "paused" => CheckStatus::Ok,
                        _ => CheckStatus::Warn,
                    };
                    checks.push(Check {
                        name: "Klipper",
                        status,
                        detail: format!(
                            "state={state}{}{}",
                            if message.is_empty() { "" } else { ": " },
                            message
                        ),
                    });
                    checks.push(Check::ok(
                        "Klipper version",
                        non_empty(&printer.software_version, "unknown"),
                    ));
                }
                Err(err) => {
                    checks.push(Check::fail(
                        "Klipper",
                        format!("printer.info failed: {}", err.public_message()),
                    ));
                }
            }
        } else {
            checks.push(Check::warn(
                "Klipper",
                "skipped because Moonraker is unreachable",
            ));
        }
    }

    if let (Some(context), true) = (&context, printer_reachable) {
        match context.camera.list().await {
            Ok(cameras) => {
                let total = cameras.len();
                let enabled = cameras.iter().filter(|camera| camera.enabled).count();
                let snapshots = cameras
                    .iter()
                    .filter(|camera| camera.snapshot_url.is_some())
                    .count();
                if total == 0 {
                    checks.push(Check::warn(
                        "cameras",
                        "none configured; camera tools will return an error until a webcam is added",
                    ));
                } else if enabled == 0 {
                    checks.push(Check::warn(
                        "cameras",
                        format!("{total} configured but all disabled"),
                    ));
                } else if snapshots == 0 {
                    checks.push(Check::warn(
                        "cameras",
                        format!("{enabled} enabled but none define a snapshot_url"),
                    ));
                } else {
                    checks.push(Check::ok(
                        "cameras",
                        format!(
                            "{total} configured, {enabled} enabled, {snapshots} with snapshots"
                        ),
                    ));
                }
            }
            Err(err) => checks.push(Check::warn(
                "cameras",
                format!("could not list webcams: {}", err.public_message()),
            )),
        }
    }

    let registry = ToolRegistry::build();
    let policy = Policy::new(config.policy.clone());
    let mut immediate = 0usize;
    let mut approval = 0usize;
    let mut denied = 0usize;
    let mut emergency = 0usize;
    for name in registry.names() {
        let Some(spec) = registry.spec(name) else {
            continue;
        };
        match spec.gate {
            Gate::Emergency => emergency += 1,
            Gate::Internal | Gate::Confirm => immediate += 1,
            Gate::Policy => match policy.evaluate(spec.capability, spec.risk) {
                Decision::Allow => immediate += 1,
                Decision::NeedsApproval => approval += 1,
                Decision::Deny(_) => denied += 1,
            },
        }
    }
    checks.push(Check::ok(
        "tools",
        format!(
            "{} registered: {immediate} immediate, {approval} approval-gated, {denied} denied by \
             policy, {emergency} emergency",
            registry.names().len()
        ),
    ));

    if let Some(context) = &context {
        match context.approvals.list_open() {
            Ok(open) => checks.push(Check::ok(
                "approvals",
                format!(
                    "mode={:?}, ttl={}s, {} pending",
                    config.approval.mode,
                    config.approval.ttl_secs,
                    open.len()
                ),
            )),
            Err(err) => checks.push(Check::warn(
                "approvals",
                format!("cannot read approval state: {}", err.public_message()),
            )),
        }
    }

    checks.push(Check::ok(
        "audit log",
        config
            .audit_log_path()
            .map_or_else(|| "stderr".to_string(), |path| path.display().to_string()),
    ));

    let failures = checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .count();
    let warnings = checks
        .iter()
        .filter(|check| check.status == CheckStatus::Warn)
        .count();
    let success = failures == 0 && (!args.strict || warnings == 0);

    if args.json {
        let report = serde_json::json!({
            "ok": success,
            "summary": {
                "ok": checks.iter().filter(|check| check.status == CheckStatus::Ok).count(),
                "warnings": warnings,
                "failures": failures,
            },
            "checks": checks.iter().map(|check| serde_json::json!({
                "name": check.name,
                "status": check.status.label(),
                "detail": check.detail,
            })).collect::<Vec<_>>(),
            "config": config.public_view(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!("mcp-moonraker doctor");
        println!("====================");
        for check in &checks {
            let marker = match check.status {
                CheckStatus::Ok => "OK  ",
                CheckStatus::Warn => "WARN",
                CheckStatus::Fail => "FAIL",
            };
            println!("[{marker}] {:<16} {}", check.name, check.detail);
        }
        println!();
        if success {
            println!("Result: OK ({warnings} warning(s))");
        } else {
            println!("Result: FAILED ({failures} failure(s), {warnings} warning(s))");
        }
    }

    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn approval_store(config: &Config) -> Result<ApprovalStore> {
    ApprovalStore::new(
        config.state_dir(),
        config.approval_ttl(),
        config.approval.mode,
    )
}

fn approve(config: &Config, approval_id: &str) -> Result<()> {
    let store = approval_store(config)?;
    let record = store.human_approve(approval_id)?;
    println!("Approved {}.", record.id);
    println!("Tool:    {}", record.tool);
    println!("Summary: {}", record.summary);
    println!("Risk:    {}", record.risk.as_str());
    if store.requires_phrase() {
        println!("Phrase:  {}", record.phrase);
        println!();
        println!(
            "The agent must now call moonraker_confirm_action with this phrase and \
             acknowledge=true."
        );
    } else {
        println!();
        println!("The agent may now call moonraker_confirm_action with acknowledge=true.");
    }
    Ok(())
}

fn deny(config: &Config, approval_id: &str) -> Result<()> {
    let store = approval_store(config)?;
    let record = store.deny(approval_id)?;
    println!("Denied {}.", record.id);
    println!("Tool:    {}", record.tool);
    println!("Summary: {}", record.summary);
    println!("The action cannot be executed.");
    Ok(())
}

fn pending(config: &Config, args: &PendingArgs) -> Result<()> {
    let store = approval_store(config)?;
    let records = if args.all {
        store.list()?
    } else {
        store.list_open()?
    };

    if args.json {
        let output = serde_json::json!({
            "count": records.len(),
            "mode": config.approval.mode,
            "approvals": records.iter().map(|record| serde_json::json!({
                "approval_id": record.id,
                "tool": record.tool,
                "summary": record.summary,
                "risk": record.risk.as_str(),
                "status": record.status,
                "created_at": record.created_at,
                "expires_at": record.expires_at,
                "expires_in_secs": record.remaining_secs(),
                "human_approved": record.human_approved_at.is_some(),
                "result": record.result,
            })).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    if records.is_empty() {
        println!("No pending approvals.");
        return Ok(());
    }

    println!(
        "{} approval(s) ({:?} mode):",
        records.len(),
        config.approval.mode
    );
    for record in &records {
        println!();
        println!("  {}", record.id);
        println!("    tool:    {}", record.tool);
        println!("    summary: {}", record.summary);
        println!("    risk:    {}", record.risk.as_str());
        println!("    status:  {:?}", record.status);
        if record.is_open() {
            println!("    expires: {}s", record.remaining_secs());
            println!("    approve: mcp-moonraker approve {}", record.id);
            println!("    deny:    mcp-moonraker deny {}", record.id);
        }
        if let Some(result) = &record.result {
            println!("    result:  {result}");
        }
    }
    Ok(())
}

fn print_config(config: &Config, args: &PrintConfigArgs) -> Result<ExitCode> {
    if args.json {
        let value = serde_json::json!({
            "secrets_omitted": true,
            "config": config,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(ExitCode::SUCCESS);
    }

    let body = toml::to_string_pretty(config).map_err(|err| {
        Error::Config(format!(
            "could not serialize the resolved configuration: {err}"
        ))
    })?;
    println!("# Resolved mcp-moonraker configuration.");
    println!("# Secrets are not printed. Supply them through environment variables.");
    print!("{body}");
    Ok(ExitCode::SUCCESS)
}

fn list_tools(args: &ToolsArgs) -> ExitCode {
    let registry = ToolRegistry::build();
    let names = registry.names();

    if args.json {
        let tools = names
            .iter()
            .filter_map(|name| registry.spec(name))
            .map(|spec| {
                serde_json::json!({
                    "name": spec.name,
                    "title": spec.title,
                    "description": spec.description,
                    "risk": spec.risk.as_str(),
                    "capability": spec.capability.as_str(),
                    "gate": gate_label(spec.gate),
                    "arguments": spec.allowed_args,
                })
            })
            .collect::<Vec<_>>();
        let output = serde_json::json!({"count": tools.len(), "tools": tools});
        println!(
            "{}",
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
        );
        return ExitCode::SUCCESS;
    }

    let width = names
        .iter()
        .map(|name| name.len())
        .max()
        .unwrap_or(0)
        .max(20);
    println!("{:<width$}  {:<8}  {:<10}  TITLE", "TOOL", "RISK", "GATE");
    println!("{}", "-".repeat(width + 44));
    for name in &names {
        let Some(spec) = registry.spec(name) else {
            continue;
        };
        println!(
            "{:<width$}  {:<8}  {:<10}  {}",
            spec.name,
            spec.risk.as_str(),
            gate_label(spec.gate),
            spec.title
        );
    }
    println!();
    println!("{} tools.", names.len());
    ExitCode::SUCCESS
}

fn gate_label(gate: Gate) -> &'static str {
    match gate {
        Gate::Policy => "policy",
        Gate::Emergency => "emergency",
        Gate::Internal => "local",
        Gate::Confirm => "confirm",
    }
}

fn init_config(args: &InitArgs) -> Result<ExitCode> {
    if args.output == "-" {
        print!("{STARTER_CONFIG}");
        return Ok(ExitCode::SUCCESS);
    }

    let path = PathBuf::from(&args.output);
    if path.exists() && !args.force {
        return Err(Error::Config(format!(
            "{} already exists; use --force to overwrite it",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&path, STARTER_CONFIG)?;

    println!("Wrote {}", path.display());
    println!();
    println!("Next steps:");
    println!("  1. Set MOONRAKER_URL if the printer is not at http://127.0.0.1:7125.");
    println!("  2. Set MOONRAKER_API_KEY only if Moonraker requires one.");
    println!("  3. Run: mcp-moonraker --config {} doctor", path.display());
    println!(
        "  4. Point your MCP client at: mcp-moonraker --config {}",
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn non_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_moonraker::{
        config::ApprovalMode,
        safety::{Capability, Risk},
    };

    #[test]
    fn starter_config_parses_and_stays_safe() {
        let config: Config = toml::from_str(STARTER_CONFIG).unwrap();
        assert_eq!(config.moonraker.url, "http://127.0.0.1:7125");
        assert_eq!(config.server.transport, TransportKind::Stdio);
        assert!(!config.policy.read_only);
        assert!(config.policy.allow_mutations);
        assert!(!config.policy.allow_raw_gcode);
        assert!(!config.policy.allow_host_admin);
        assert_eq!(config.approval.mode, ApprovalMode::InBand);
        assert!(config.moonraker.api_key.is_none());
        assert!(config.server.http.token.is_none());
    }

    #[test]
    fn starter_config_contains_no_secret_placeholders() {
        let lower = STARTER_CONFIG.to_ascii_lowercase();
        assert!(!lower.contains("hunter2"));
        assert!(!lower.contains("changeme"));
        assert!(!lower.contains("api_key ="));
        assert!(!lower.contains("token ="));
    }

    #[test]
    fn serve_overrides_revalidate() {
        let mut config = Config::default();
        let args = ServeArgs {
            transport: Some("http".to_string()),
            http_host: Some("0.0.0.0".to_string()),
            ..ServeArgs::default()
        };
        // Non-loopback HTTP without a token must be rejected.
        assert!(args.apply(&mut config).is_err());
    }

    #[test]
    fn tool_gates_are_labelled() {
        assert_eq!(gate_label(Gate::Policy), "policy");
        assert_eq!(gate_label(Gate::Emergency), "emergency");
        assert_eq!(gate_label(Gate::Internal), "local");
        assert_eq!(gate_label(Gate::Confirm), "confirm");
    }

    #[test]
    fn risk_and_capability_labels_are_stable() {
        assert_eq!(Risk::Critical.as_str(), "critical");
        assert_eq!(Capability::RawGcode.as_str(), "raw_gcode");
    }
}
