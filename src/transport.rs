//! Transport startup: stdio (default) and optional streamable HTTP/SSE.
//!
//! stdio is the zero-friction option for local agents: the client spawns the
//! binary and speaks MCP over stdin/stdout. Nothing except MCP messages is ever
//! written to stdout; logs and approval prompts go to stderr.
//!
//! HTTP is opt-in. It binds to loopback by default. A non-loopback bind requires
//! a static bearer token (enforced by [`crate::config`]), and DNS-rebinding
//! protection stays enabled.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use rust_mcp_axum::{create_axum_server, AxumServerOptions};
use rust_mcp_sdk::{
    auth::{AuthInfo, AuthProvider, AuthenticationError, OauthEndpoint},
    error::SdkResult,
    event_store::InMemoryEventStore,
    mcp_http::{DnsRebindingOptions, GenericBody, McpAppState, McpHttpError},
    mcp_server::{server_runtime, McpServerOptions, ToMcpServerHandler},
    schema::InitializeResult,
    McpServer, StdioTransport, TransportOptions,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::{
    config::Config,
    error::{Error, Result},
    server::MoonrakerServerHandler,
};

/// Run the MCP server over stdio until the client disconnects.
pub async fn run_stdio(
    config: &Config,
    handler: MoonrakerServerHandler,
    server_details: InitializeResult,
) -> Result<()> {
    let _ = config;
    let transport = StdioTransport::new(TransportOptions::default())
        .map_err(|err| Error::Config(format!("could not start stdio transport: {err}")))?;
    let server = server_runtime::create_server(McpServerOptions {
        server_details,
        transport,
        handler: handler.to_mcp_server_handler(),
        task_store: None,
        client_task_store: None,
        message_observer: None,
    });
    server
        .start()
        .await
        .map_err(|err| Error::Config(format!("MCP stdio server stopped: {err}")))?;
    Ok(())
}

/// Run the MCP server over streamable HTTP (with SSE compatibility).
pub async fn run_http(
    config: &Config,
    handler: MoonrakerServerHandler,
    server_details: InitializeResult,
) -> Result<()> {
    let http = &config.server.http;
    let auth = http
        .token
        .as_ref()
        .map(|token| Arc::new(StaticTokenAuth::new(token)) as Arc<dyn AuthProvider>);

    let allowed_hosts = (!http.allowed_hosts.is_empty()).then(|| http.allowed_hosts.clone());
    let allowed_origins = (!http.allowed_origins.is_empty()).then(|| http.allowed_origins.clone());

    let options = AxumServerOptions {
        host: http.host.clone(),
        port: http.port,
        custom_streamable_http_endpoint: Some(http.path.clone()),
        custom_sse_endpoint: Some("/sse".to_string()),
        custom_messages_endpoint: Some("/messages".to_string()),
        sse_support: true,
        health_endpoint: Some("/health".to_string()),
        max_request_body_size: Some(4 * 1024 * 1024),
        enable_json_response: Some(false),
        event_store: Some(Arc::new(InMemoryEventStore::default())),
        auth,
        dns_rebinding: DnsRebindingOptions {
            allowed_hosts,
            allowed_origins,
            ..Default::default()
        },
        ..Default::default()
    };

    tracing::info!(
        target: "mcp_moonraker::transport",
        host = %http.host,
        port = http.port,
        path = %http.path,
        authenticated = http.token.is_some(),
        "starting streamable HTTP transport"
    );

    let server = create_axum_server(server_details, handler.to_mcp_server_handler(), options);
    server.start().await.map_err(sdk_error)?;
    Ok(())
}

fn sdk_error(err: impl std::fmt::Display) -> Error {
    Error::Config(format!("MCP HTTP server stopped: {err}"))
}

/// Static bearer-token authentication for the HTTP transport.
///
/// This is deliberately simple: one token, compared in constant time via
/// SHA-256 digests. It is suitable for a trusted LAN or a reverse proxy that
/// terminates TLS. For internet-facing deployments, put an OAuth-capable proxy
/// in front instead.
struct StaticTokenAuth {
    token_hash: [u8; 32],
}

impl StaticTokenAuth {
    fn new(token: &SecretString) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(token.expose_secret().as_bytes());
        Self {
            token_hash: hasher.finalize().into(),
        }
    }
}

#[async_trait]
impl AuthProvider for StaticTokenAuth {
    async fn verify_token(
        &self,
        access_token: String,
    ) -> std::result::Result<AuthInfo, AuthenticationError> {
        let mut hasher = Sha256::new();
        hasher.update(access_token.as_bytes());
        let presented: [u8; 32] = hasher.finalize().into();
        if !constant_time_eq(&presented, &self.token_hash) {
            return Err(AuthenticationError::InvalidToken {
                description: "invalid bearer token",
            });
        }

        // The SDK's middleware rejects AuthInfo without an expiry. A static
        // token has no natural expiry, so use a far-future value and let the
        // operator rotate it manually.
        let expires_at = SystemTime::now() + Duration::from_secs(10 * 365 * 24 * 60 * 60);
        Ok(AuthInfo {
            token_unique_id: "mcp-moonraker-static-token".to_string(),
            client_id: Some("mcp-moonraker-client".to_string()),
            user_id: None,
            scopes: None,
            expires_at: Some(expires_at),
            audience: None,
            extra: None,
        })
    }

    fn auth_endpoints(&self) -> Option<&HashMap<String, OauthEndpoint>> {
        None
    }

    async fn handle_request(
        &self,
        _request: http::Request<&str>,
        _state: Arc<McpAppState>,
    ) -> std::result::Result<http::Response<GenericBody>, McpHttpError> {
        Err(McpHttpError::HttpError(
            "this server uses a static bearer token and has no OAuth endpoints".to_string(),
        ))
    }

    fn protected_resource_metadata_url(&self) -> Option<&str> {
        None
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Verify the SDK result type is still wired as expected.
#[allow(dead_code)]
fn _sdk_result(_value: SdkResult<()>) {}
