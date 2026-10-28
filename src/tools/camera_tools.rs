//! Camera tools.
//!
//! `moonraker_camera_snapshot` returns an MCP `image` content block, so a client
//! that renders images shows the current frame directly. A short text block
//! carries the camera name, MIME type, byte size, and capture time.

use rust_mcp_sdk::schema::{CallToolResult, ContentBlock, ImageContent, TextContent};
use serde_json::{json, Value};

use crate::{
    safety::{Capability, Risk},
    tools::{
        args::Args, empty_schema, image_result, json_result, no_summary, schema_object, Gate,
        ToolContext, ToolSpec,
    },
};

/// Camera tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_cameras",
            title: "List configured webcams",
            description: "List webcams configured in Moonraker, including whether each is enabled \
                          and whether a snapshot URL is available. Use this before requesting a \
                          snapshot if the camera name is unknown.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: empty_schema,
            handler: |ctx, _args| Box::pin(cameras(ctx)),
        },
        ToolSpec {
            name: "moonraker_camera_snapshot",
            title: "Current camera frame",
            description: "Capture and return the current frame from a Moonraker webcam as an MCP \
                          image. If exactly one camera is enabled, 'camera' may be omitted. With \
                          multiple enabled cameras, pass the camera name explicitly.",
            allowed_args: &["camera", "max_bytes"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: snapshot_schema,
            handler: |ctx, args| Box::pin(camera_snapshot(ctx, args)),
        },
    ]
}

async fn cameras(ctx: &ToolContext) -> Result<CallToolResult, crate::Error> {
    let webcams = ctx.camera.list().await?;
    let cameras = webcams
        .iter()
        .map(|camera| {
            json!({
                "name": camera.name,
                "enabled": camera.enabled,
                "service": camera.service,
                "location": camera.location,
                "stream_url": camera.stream_url.as_deref().map(crate::safety::redact::redact_url_userinfo),
                "snapshot_url": camera.snapshot_url.as_deref().map(crate::safety::redact::redact_url_userinfo),
                "target_fps": camera.target_fps,
                "target_fps_idle": camera.target_fps_idle,
                "aspect_ratio": camera.aspect_ratio,
            })
        })
        .collect::<Vec<_>>();

    Ok(json_result(
        &json!({"count": cameras.len(), "cameras": cameras}),
        None,
    ))
}

fn snapshot_schema() -> Value {
    schema_object(
        json!({
            "camera": {
                "type": "string",
                "description": "Camera name from moonraker_cameras. Required when more than one camera is enabled."
            },
            "max_bytes": {
                "type": "integer",
                "minimum": 1024,
                "maximum": 67_108_864,
                "description": "Optional cap for this snapshot in bytes. Cannot exceed the server's configured camera limit."
            }
        }),
        &[],
    )
}

async fn camera_snapshot(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<CallToolResult, crate::Error> {
    let args = Args::parse(Some(arguments), &["camera", "max_bytes"])?;
    let camera = args.optional_string("camera")?;
    let max_bytes = args
        .optional_u64("max_bytes")?
        .map(|value| value.clamp(1024, 64 * 1024 * 1024) as usize);

    let snapshot = ctx.camera.snapshot(camera.as_deref(), max_bytes).await?;
    let metadata = json!({
        "camera": snapshot.camera,
        "mime_type": snapshot.mime_type,
        "bytes": snapshot.bytes.len(),
        "captured_at_unix": snapshot.captured_at,
        "source_url": snapshot.source_url,
    });
    let metadata_text = serde_json::to_string_pretty(&metadata).unwrap_or_default();

    Ok(CallToolResult {
        content: vec![
            ContentBlock::TextContent(TextContent::from(metadata_text)),
            ContentBlock::ImageContent(ImageContent::new(
                snapshot.base64(),
                snapshot.mime_type,
                None,
                None,
            )),
        ],
        is_error: None,
        meta: None,
        structured_content: None,
    })
}

/// Re-export for tests that only need an image result.
#[must_use]
pub fn single_image(data: String, mime_type: String) -> CallToolResult {
    image_result(data, mime_type)
}
