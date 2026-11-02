//! Async Moonraker client.
//!
//! Moonraker exposes nearly everything over JSON-RPC 2.0 at
//! `POST /server/jsonrpc`, and file downloads over plain HTTP. Using JSON-RPC
//! for everything else keeps argument typing consistent and avoids the HTTP
//! query-string type-hint rules.
//!
//! Connection details:
//! * API key (optional) is sent as `X-Api-Key`.
//! * The key is only forwarded to camera URLs on the same origin as Moonraker.
//! * Responses are size-limited to avoid memory exhaustion from a hostile or
//!   misbehaving backend.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{
    header::{HeaderMap, HeaderValue, CONTENT_TYPE, USER_AGENT},
    Client, RequestBuilder, StatusCode,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use url::Url;

use crate::{
    config::MoonrakerConfig,
    error::{Error, Result},
};

const USER_AGENT_VALUE: &str = concat!("mcp-moonraker/", env!("CARGO_PKG_VERSION"));

/// Thin, cloneable Moonraker client.
#[derive(Clone)]
pub struct MoonrakerClient {
    http: Client,
    base: Url,
    api_key: Option<SecretString>,
    max_response_bytes: usize,
    /// Per-request budget for `printer.gcode.script` calls, which only return
    /// once the script has finished executing.
    gcode_timeout: Duration,
    request_id: std::sync::Arc<AtomicU64>,
}

impl std::fmt::Debug for MoonrakerClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MoonrakerClient")
            .field("base", &self.base)
            .field("api_key_configured", &self.api_key.is_some())
            .field("max_response_bytes", &self.max_response_bytes)
            .field("gcode_timeout_secs", &self.gcode_timeout.as_secs())
            .finish_non_exhaustive()
    }
}

impl MoonrakerClient {
    /// Build a client from configuration.
    pub fn new(config: &MoonrakerConfig) -> Result<Self> {
        let base = Url::parse(&config.url)?;
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        let http = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .connect_timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(!config.tls_verify)
            .default_headers(headers)
            .build()
            .map_err(Error::Http)?;

        Ok(Self {
            http,
            base,
            api_key: config.api_key.clone(),
            max_response_bytes: config.max_response_bytes,
            gcode_timeout: Duration::from_secs(config.script_timeout_secs),
            request_id: std::sync::Arc::new(AtomicU64::new(1)),
        })
    }

    /// Base URL.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// Whether an API key is configured.
    #[must_use]
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Perform a JSON-RPC call and deserialize the `result` object.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T> {
        let value = self.call_value(method, params).await?;
        serde_json::from_value(value).map_err(Error::Json)
    }

    /// Perform a JSON-RPC call and return the raw `result` value.
    pub async fn call_value(&self, method: &str, params: Option<Value>) -> Result<Value> {
        self.call_value_with_timeout(method, params, None).await
    }

    /// Perform a JSON-RPC call with a per-request timeout override.
    ///
    /// `Some(duration)` replaces the client-wide timeout for this single call.
    pub async fn call_value_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let mut body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "id": id,
        });
        if let Some(params) = params {
            body["params"] = params;
        }

        let mut request = self
            .authorized(self.http.post(self.endpoint("/server/jsonrpc")))
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }

        let response = request
            .send()
            .await
            .map_err(|err| Error::Http(err.without_url()))?;

        let status = response.status();
        let bytes = read_limited(response, self.max_response_bytes).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
            Error::InvalidResponse(format!(
                "Moonraker returned non-JSON data (HTTP {}): {err}",
                status.as_u16()
            ))
        })?;

        if let Some(error) = value.get("error") {
            return Err(jsonrpc_error(error, status));
        }

        value.get("result").cloned().ok_or_else(|| {
            Error::InvalidResponse("JSON-RPC response did not contain a result".to_string())
        })
    }

    /// Execute a G-code script and wait for Klipper to finish running it.
    ///
    /// Klipper only answers `printer.gcode.script` once the script has been
    /// fully executed, so motion-heavy scripts (e.g. `G28` homing) and macros
    /// that block (e.g. `TEMPERATURE_WAIT`) can run far longer than a normal
    /// request. These calls use the dedicated `moonraker.script_timeout_secs`
    /// budget instead of `moonraker.timeout_secs`.
    pub async fn call_gcode_script(&self, script: &str) -> Result<Value> {
        self.call_value_with_timeout(
            "printer.gcode.script",
            Some(json!({"script": script})),
            Some(self.gcode_timeout),
        )
        .await
    }

    /// Call a REST endpoint that returns `{"result": ...}`.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let value = self.get_value(path, query).await?;
        serde_json::from_value(value).map_err(Error::Json)
    }

    /// Call a REST endpoint and return the raw `result` value, or the body when
    /// the endpoint is not wrapped in a `result` object.
    pub async fn get_value(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        let mut url = self.endpoint(path);
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }

        let response = self
            .authorized(self.http.get(url))
            .send()
            .await
            .map_err(|err| Error::Http(err.without_url()))?;
        let status = response.status();
        let bytes = read_limited(response, self.max_response_bytes).await?;
        if !status.is_success() {
            return Err(http_error(status, &bytes));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
            Error::InvalidResponse(format!(
                "Moonraker returned non-JSON data (HTTP {}): {err}",
                status.as_u16()
            ))
        })?;
        Ok(value.get("result").cloned().unwrap_or(value))
    }

    /// Download a path under `/server/files/...`.
    pub async fn download_file(&self, root: &str, path: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let encoded_root = url_encode(root);
        let encoded_path = path
            .split('/')
            .map(url_encode)
            .collect::<Vec<_>>()
            .join("/");
        let url = self.endpoint(&format!("/server/files/{encoded_root}/{encoded_path}"));
        let response = self
            .authorized(self.http.get(url))
            .send()
            .await
            .map_err(|err| Error::Http(err.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            let bytes = read_limited(response, self.max_response_bytes).await?;
            return Err(http_error(status, &bytes));
        }
        read_limited(response, max_bytes).await
    }

    /// Fetch bytes from an arbitrary URL (used for camera snapshots).
    ///
    /// `send_api_key` should only be true when the URL has the exact same
    /// origin as the Moonraker base URL.
    pub async fn fetch_bytes(
        &self,
        url: &Url,
        timeout: Duration,
        max_bytes: usize,
        send_api_key: bool,
    ) -> Result<(Vec<u8>, Option<String>)> {
        let mut request = self.http.get(url.clone()).timeout(timeout);
        if send_api_key && self.same_origin(url) {
            request = self.authorized(request);
        }
        let response = request
            .send()
            .await
            .map_err(|err| Error::Http(err.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Camera(format!(
                "camera returned HTTP {}",
                status.as_u16()
            )));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_string());
        if let Some(length) = response.content_length() {
            if length > max_bytes as u64 {
                return Err(Error::Camera(format!(
                    "camera image is too large ({length} bytes, limit {max_bytes})"
                )));
            }
        }
        let bytes = read_limited(response, max_bytes).await?;
        Ok((bytes, content_type))
    }

    /// Whether `url` shares scheme, host, and port with the Moonraker base URL.
    #[must_use]
    pub fn same_origin(&self, url: &Url) -> bool {
        self.base.scheme() == url.scheme()
            && self.base.host_str() == url.host_str()
            && self.base.port_or_known_default() == url.port_or_known_default()
    }

    /// Build an absolute URL for a path relative to the base URL.
    pub fn join(&self, path: &str) -> Result<Url> {
        self.base.join(path).map_err(Error::Url)
    }

    fn endpoint(&self, path: &str) -> Url {
        let trimmed = path.trim_start_matches('/');
        // `Url::join` treats a leading slash as absolute; the base may include
        // a path prefix for reverse-proxy deployments, so preserve it.
        let base_path = self.base.path().trim_end_matches('/');
        let mut url = self.base.clone();
        url.set_path(&format!("{base_path}/{trimmed}"));
        url
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(key) = &self.api_key {
            request.header("X-Api-Key", key.expose_secret())
        } else {
            request
        }
    }
}

async fn read_limited(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>> {
    if let Some(length) = response.content_length() {
        if length > max_bytes as u64 {
            return Err(Error::InvalidResponse(format!(
                "response is too large ({length} bytes, limit {max_bytes})"
            )));
        }
    }

    let mut stream = response.bytes_stream();
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if output.len() + chunk.len() > max_bytes {
            return Err(Error::InvalidResponse(format!(
                "response exceeded the {max_bytes} byte limit"
            )));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

fn jsonrpc_error(error: &Value, status: StatusCode) -> Error {
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or(i64::from(status.as_u16()));
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown Moonraker error")
        .to_string();
    Error::api(code, message)
}

fn http_error(status: StatusCode, body: &[u8]) -> Error {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        if let Some(error) = value.get("error") {
            if let Some(message) = error.get("message").and_then(Value::as_str) {
                return Error::api(
                    error
                        .get("code")
                        .and_then(Value::as_i64)
                        .unwrap_or(i64::from(status.as_u16())),
                    message.to_string(),
                );
            }
            if let Some(message) = error.as_str() {
                return Error::api(i64::from(status.as_u16()), message.to_string());
            }
        }
    }
    Error::api(
        i64::from(status.as_u16()),
        format!("Moonraker returned HTTP {}", status.as_u16()),
    )
}

fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Exact `{"result": ...}` wrapper for endpoints that use it.
#[derive(Debug, Deserialize)]
pub struct Wrapped<T> {
    pub result: T,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{body_partial_json, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn client(base: &str) -> MoonrakerClient {
        MoonrakerClient::new(&MoonrakerConfig {
            url: base.to_string(),
            api_key: Some(SecretString::from("test-key")),
            timeout_secs: 5,
            script_timeout_secs: 5,
            tls_verify: true,
            max_response_bytes: 1024 * 1024,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn sends_api_key_and_parses_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/server/jsonrpc"))
            .and(header("X-Api-Key", "test-key"))
            .and(body_partial_json(json!({"method": "server.info"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"klippy_state": "ready"}
            })))
            .mount(&server)
            .await;

        let value: Value = client(&server.uri())
            .call_value("server.info", None)
            .await
            .unwrap();
        assert_eq!(value["klippy_state"], "ready");
    }

    #[tokio::test]
    async fn maps_jsonrpc_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/server/jsonrpc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": 400, "message": "boom"}
            })))
            .mount(&server)
            .await;

        let error = client(&server.uri())
            .call_value("server.info", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("boom"));
    }

    /// A `printer.gcode.script` call only replies once the script finishes, so
    /// it must use the dedicated script timeout instead of the normal request
    /// timeout — this is what keeps `G28` homing from reporting a spurious
    /// timeout while the homing still completes on the printer.
    #[tokio::test]
    async fn gcode_script_overrides_request_timeout() {
        let server = MockServer::start().await;
        // 2 s delay: longer than the client's 1 s normal timeout.
        Mock::given(method("POST"))
            .and(path("/server/jsonrpc"))
            .and(body_partial_json(json!({"method": "printer.gcode.script"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {"ok": true}
                    }))
                    .set_delay(Duration::from_secs(2)),
            )
            .mount(&server)
            .await;
        let config = MoonrakerConfig {
            url: server.uri(),
            api_key: None,
            timeout_secs: 1,
            script_timeout_secs: 30,
            tls_verify: true,
            max_response_bytes: 1024 * 1024,
        };
        let slow_client = MoonrakerClient::new(&config).unwrap();

        // Normal RPC: cancelled by the 1 s request timeout.
        assert!(slow_client.call_value("server.info", None).await.is_err());
        // Same call shape as a script: allowed to run to completion.
        let value = slow_client.call_gcode_script("G28 X Y Z").await.unwrap();
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn preserves_reverse_proxy_prefix() {
        let client = MoonrakerClient::new(&MoonrakerConfig {
            url: "http://printer.local/moonraker".into(),
            ..MoonrakerConfig::default()
        })
        .unwrap();
        assert_eq!(
            client.endpoint("/server/info").as_str(),
            "http://printer.local/moonraker/server/info"
        );
    }

    #[tokio::test]
    async fn same_origin_ignores_path() {
        let client = MoonrakerClient::new(&MoonrakerConfig::default()).unwrap();
        assert!(client.same_origin(&Url::parse("http://127.0.0.1:7125/webcam/snapshot").unwrap()));
        assert!(!client.same_origin(&Url::parse("http://camera.local/snapshot").unwrap()));
    }
}
