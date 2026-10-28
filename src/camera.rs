//! Camera discovery and snapshot retrieval.
//!
//! Moonraker owns the webcam configuration (`server.webcams.list`). We only
//! fetch URLs that Moonraker returned, which keeps the trust boundary clear:
//! the camera list is as trusted as the Moonraker instance itself.
//!
//! Extra hardening:
//! * only `http`/`https` URLs;
//! * optional allow-list of camera hosts;
//! * optional rejection of private/LAN destinations;
//! * explicit block on cloud metadata endpoints;
//! * byte cap, timeout, and image content-type check.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use url::Url;

use crate::{
    config::CameraConfig,
    error::{Error, Result},
    moonraker::{
        client::MoonrakerClient,
        types::{Webcam, WebcamList},
    },
    safety::redact::redact_url_userinfo,
};

/// A retrieved camera frame.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub camera: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub captured_at: u64,
    pub source_url: String,
}

impl Snapshot {
    /// Base64 payload expected by MCP image content.
    #[must_use]
    pub fn base64(&self) -> String {
        BASE64.encode(&self.bytes)
    }
}

/// Camera service backed by the Moonraker webcam list.
#[derive(Debug, Clone)]
pub struct CameraService {
    client: MoonrakerClient,
    config: CameraConfig,
}

impl CameraService {
    /// Create a service.
    #[must_use]
    pub fn new(client: MoonrakerClient, config: CameraConfig) -> Self {
        Self { client, config }
    }

    /// List configured webcams.
    pub async fn list(&self) -> Result<Vec<Webcam>> {
        let list: WebcamList = self.client.call("server.webcams.list", None).await?;
        Ok(list.webcams)
    }

    /// Capture a snapshot from a named camera, or the only enabled camera.
    ///
    /// If more than one camera is enabled and no name is supplied, this returns
    /// an error listing the choices rather than guessing.
    pub async fn snapshot(
        &self,
        camera: Option<&str>,
        max_bytes_override: Option<usize>,
    ) -> Result<Snapshot> {
        let webcams = self.list().await?;
        if webcams.is_empty() {
            return Err(Error::Camera(
                "Moonraker has no webcams configured; add a [webcam] section to moonraker.conf \
                 or use the webcam component"
                    .to_string(),
            ));
        }

        let selected = select_camera(&webcams, camera)?;
        if !selected.enabled {
            return Err(Error::Camera(format!(
                "camera '{}' is disabled in Moonraker",
                selected.name
            )));
        }

        let snapshot_url = selected.snapshot_url.as_deref().ok_or_else(|| {
            Error::Camera(format!(
                "camera '{}' does not define a snapshot_url; add one to its Moonraker webcam \
                 configuration (for example /webcam/?action=snapshot)",
                selected.name
            ))
        })?;

        let url = self.resolve_url(snapshot_url)?;
        self.validate_destination(&url).await?;

        let max_bytes = max_bytes_override
            .unwrap_or(self.config.max_bytes)
            .min(self.config.max_bytes);
        let (bytes, content_type) = self
            .client
            .fetch_bytes(
                &url,
                Duration::from_secs(self.config.timeout_secs),
                max_bytes,
                true,
            )
            .await?;

        if bytes.is_empty() {
            return Err(Error::Camera(format!(
                "camera '{}' returned an empty snapshot",
                selected.name
            )));
        }

        let mime_type = content_type
            .as_deref()
            .filter(|value| value.starts_with("image/"))
            .map(str::to_string)
            .or_else(|| sniff_image_mime(&bytes).map(str::to_string))
            .ok_or_else(|| {
                Error::Camera(format!(
                    "camera '{}' returned an unsupported content type: {}",
                    selected.name,
                    content_type.as_deref().unwrap_or("unknown")
                ))
            })?;

        if !is_supported_image(&mime_type) {
            return Err(Error::Camera(format!(
                "camera '{}' returned unsupported image type '{mime_type}'",
                selected.name
            )));
        }

        Ok(Snapshot {
            camera: selected.name.clone(),
            mime_type,
            bytes,
            captured_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            source_url: redact_url_userinfo(url.as_str()),
        })
    }

    fn resolve_url(&self, value: &str) -> Result<Url> {
        if value.starts_with("http://") || value.starts_with("https://") {
            Url::parse(value).map_err(Error::Url)
        } else {
            self.client.join(value)
        }
    }

    async fn validate_destination(&self, url: &Url) -> Result<()> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::Camera(
                "camera snapshot URL must use http:// or https://".to_string(),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| Error::Camera("camera snapshot URL has no host".to_string()))?;

        if is_metadata_host(host) {
            return Err(Error::Camera(
                "refusing to fetch a cloud metadata endpoint".to_string(),
            ));
        }

        if !self.config.allowed_hosts.is_empty()
            && !self
                .config
                .allowed_hosts
                .iter()
                .any(|allowed| host_matches(host, allowed))
        {
            return Err(Error::Camera(format!(
                "camera host '{host}' is not in MCP_MOONRAKER_CAMERA_ALLOWED_HOSTS"
            )));
        }

        if !self.config.allow_private_hosts && is_private_destination(url, host).await {
            return Err(Error::Camera(
                "camera resolves to a private/loopback address; set \
                 MCP_MOONRAKER_CAMERA_ALLOW_PRIVATE_HOSTS=true if this is intentional"
                    .to_string(),
            ));
        }

        Ok(())
    }
}

fn select_camera<'a>(webcams: &'a [Webcam], name: Option<&str>) -> Result<&'a Webcam> {
    if let Some(name) = name {
        let wanted = name.trim();
        if wanted.is_empty() {
            return Err(Error::Camera("camera name must not be empty".to_string()));
        }
        return webcams
            .iter()
            .find(|camera| camera.name == wanted)
            .or_else(|| {
                webcams
                    .iter()
                    .find(|camera| camera.name.eq_ignore_ascii_case(wanted))
            })
            .ok_or_else(|| {
                let names = webcams
                    .iter()
                    .map(|camera| camera.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Error::Camera(format!(
                    "camera '{wanted}' not found; available cameras: {names}"
                ))
            });
    }

    let enabled: Vec<_> = webcams.iter().filter(|camera| camera.enabled).collect();
    match enabled.as_slice() {
        [] => Err(Error::Camera(
            "all configured cameras are disabled in Moonraker".to_string(),
        )),
        [only] => Ok(only),
        many => {
            let names = many
                .iter()
                .map(|camera| camera.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::Camera(format!(
                "multiple cameras are enabled ({names}); pass the camera name explicitly"
            )))
        }
    }
}

fn is_supported_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/jpeg" | "image/jpg" | "image/png" | "image/webp" | "image/gif"
    )
}

fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    None
}

fn is_metadata_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "metadata.google.internal"
        || lower == "metadata.google.internal."
        || lower == "instance-data"
        || lower == "169.254.169.254"
        || lower == "100.100.100.200"
        || lower == "[fd00:ec2::254]"
        || lower == "fd00:ec2::254"
}

fn host_matches(host: &str, pattern: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        host == pattern
    }
}

async fn is_private_destination(url: &Url, host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_private_ip(ip);
    }
    let port = url.port_or_known_default().unwrap_or(80);
    match tokio::net::lookup_host((host, port)).await {
        Ok(addresses) => addresses.map(|address| address.ip()).any(is_private_ip),
        Err(_) => true, // fail closed
    }
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_private_ipv4(ip),
        IpAddr::V6(ip) => is_private_ipv6(ip),
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.octets()[0] == 0
        || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])) // CGNAT
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || ip.is_multicast()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(name: &str, enabled: bool) -> Webcam {
        Webcam {
            name: name.to_string(),
            enabled,
            ..Webcam::default()
        }
    }

    #[test]
    fn selects_single_enabled_camera() {
        let cameras = vec![camera("a", true), camera("b", false)];
        assert_eq!(select_camera(&cameras, None).unwrap().name, "a");
    }

    #[test]
    fn refuses_ambiguous_selection() {
        let cameras = vec![camera("a", true), camera("b", true)];
        assert!(select_camera(&cameras, None).is_err());
        assert_eq!(select_camera(&cameras, Some("B")).unwrap().name, "b");
    }

    #[test]
    fn sniffs_common_images() {
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0x00]),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_image_mime(b"\x89PNG\r\n\x1a\n\x00"),
            Some("image/png")
        );
        assert_eq!(sniff_image_mime(b"not an image"), None);
    }

    #[test]
    fn blocks_metadata_hosts() {
        assert!(is_metadata_host("169.254.169.254"));
        assert!(is_metadata_host("metadata.google.internal"));
        assert!(!is_metadata_host("printer.local"));
    }

    #[test]
    fn wildcard_hosts_work() {
        assert!(host_matches("cam.printer.lan", "*.printer.lan"));
        assert!(host_matches("printer.lan", "*.printer.lan"));
        assert!(!host_matches("evilprinter.lan", "*.printer.lan"));
    }
}
