# mcp-moonraker

A safe, structured [Model Context Protocol](https://modelcontextprotocol.io) server for
[Moonraker](https://moonraker.readthedocs.io) and Klipper.

It gives an AI agent the printer context it needs — state, temperatures, camera frames,
files, history, and diagnostics — plus a deliberately small set of controlled actions.
Anything destructive requires a two-step, single-use approval instead of running on the
first request.

- **47 tools** for status, cameras, heaters, motion, files, job queue, print control, host
  administration, and safety.
- **Irreversible actions are off or approval-gated.** Raw G-code, host administration, and
  config writes are disabled by default; emergency stop is always available.
- **The approved action is the executed action.** Confirmation replays the stored tool and
  arguments; callers cannot swap them.
- **No Klipper or Moonraker changes required** for normal local use.
- **One binary, one config file.** stdio by default, optional authenticated streamable HTTP.
- **Secrets stay out of output.** Environment-first secrets, recursive redaction, and
  secret-free `doctor` / `print-config` output.

## Quick start

Requires Rust 1.89+ and a reachable Moonraker (default `http://127.0.0.1:7125`).

```bash
git clone https://github.com/martinsgmx/mcp-moonraker.git
cd mcp-moonraker
cargo install --path .
mcp-moonraker doctor
```

`doctor` checks the configuration, Moonraker, Klipper, cameras, tools, and approvals. For a
printer on another host:

```bash
mcp-moonraker doctor --url http://printer.local:7125
```

Then start the server with no arguments and point your MCP client at the binary; it speaks
stdio.

### Client configuration

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "moonraker": {
      "command": "/home/you/.cargo/bin/mcp-moonraker",
      "env": { "MOONRAKER_URL": "http://192.168.1.50:7125" }
    }
  }
}
```

OpenCode (`opencode.json`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "moonraker": {
      "type": "local",
      "command": ["mcp-moonraker"],
      "environment": { "MOONRAKER_URL": "http://192.168.1.50:7125" }
    }
  }
}
```

For other clients, run `mcp-moonraker` with no arguments and use stdio. If the client starts
processes with a minimal environment, set `MOONRAKER_URL` explicitly and use an absolute
path to the binary. More examples are in [`examples/`](examples/).

## Configuration

Sources are applied in order, later winning: built-in defaults → TOML file → environment →
CLI flags. Generate a documented starter file:

```bash
mcp-moonraker init --output mcp-moonraker.toml
mcp-moonraker --config mcp-moonraker.toml doctor
```

Keep secrets in the environment: `MOONRAKER_API_KEY` (only if Moonraker requires one) and
`MCP_MOONRAKER_HTTP_TOKEN` (HTTP transport only). The TOML file contains no secrets and is
never required.

Defaults that matter:

| Setting | Default | Why |
| --- | --- | --- |
| Mutations | enabled | Heaters, fans, XY jogs, and pause/resume are useful and reversible |
| Raw G-code | **disabled** | Bypasses every structured guard |
| Host administration | **disabled** | Reboot/shutdown/update have a large blast radius |
| Config writes | **disabled** | Not implemented in v0.1 |
| Emergency stop | **enabled** | Works even in read-only mode |
| Approval mode | `in_band` | Convenient for a trusted local agent |
| HTTP bind / auth | `127.0.0.1:8765`, token required off-loopback | Never exposed by accident |

For an autonomous or remote agent, use `out_of_band` or `both` approval mode so a human must
run `mcp-moonraker approve <id>` before a destructive action executes. Every key and
environment variable is documented in [docs/configuration.md](docs/configuration.md).

## Safety model

Every tool declares a capability and risk level. A single registry enforces the policy,
creates approvals, executes handlers, and writes audit records; no tool bypasses it.

High/critical actions return an approval id instead of acting. The agent then calls
`moonraker_confirm_action` with that id, the confirmation phrase, and `acknowledge: true`.
Approvals are single-use, expire after 120 seconds, and are stored with `0700`/`0600`
permissions. A human approval through the CLI is added in `out_of_band` and `both` modes.

See [docs/safety.md](docs/safety.md) for the policy matrix, approval flow, and validation
rules, and [SECURITY.md](SECURITY.md) for the threat model.

## Cameras

Webcams already configured in Moonraker are discovered through `server.webcams.list`, and
`moonraker_camera_snapshot` returns the current frame as an MCP `image` block. No extra
Moonraker configuration is needed beyond a working webcam.

LAN/private camera hosts are allowed by default; cloud metadata endpoints are always
blocked. Set `camera.allow_private_hosts = false` or `camera.allowed_hosts` to harden this.

## HTTP transport

stdio is the right choice for a local agent. Use HTTP only when the client cannot launch a
process:

```bash
export MCP_MOONRAKER_HTTP_TOKEN="$(openssl rand -hex 32)"
mcp-moonraker serve --transport http --http-host 127.0.0.1 --http-port 8765
```

The endpoint is `POST /mcp` (streamable HTTP, SSE-compatible), with unauthenticated
`/health` for probes. Binding to a non-loopback address without a token is rejected. Put
TLS and rate limiting in a reverse proxy before exposing it. See
[docs/deployment.md](docs/deployment.md).

## Compatibility

- Moonraker v0.9+ (developed against v0.11.0 / API 1.5.0) with the standard HTTP API enabled.
- MCP protocol `2025-11-25` via `rust-mcp-sdk` 1.1.0. The tool layer is isolated from the SDK
  so a future protocol revision can be adopted without changing tools.
- `printer.cfg`, `moonraker.conf`, and service units do not need changes for a trusted local
  client. If Moonraker requires authentication, set `MOONRAKER_API_KEY`.

## Troubleshooting

- **Unreachable:** check `MOONRAKER_URL` and `curl http://127.0.0.1:7125/server/info`. A
  container needs the host's LAN address, not `127.0.0.1`.
- **No tools appear:** run with `MCP_MOONRAKER_LOG_LEVEL=mcp_moonraker=debug` (logs go to
  stderr only), use an absolute binary path, and check `MCP_MOONRAKER_CONFIG`.
- **Action disabled:** local policy, not Moonraker. Check `mcp-moonraker tools` or
  `moonraker_policy_info`.
- **Camera fails:** check `snapshot_url` from `moonraker_cameras` and fetch it from the MCP
  host. `localhost` URLs from a container point at the container.
- **HTTP 401:** send `Authorization: Bearer <token>`.

## Development

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
python3 scripts/smoke_mcp.py target/debug/mcp-moonraker
```

The smoke test is read-only and needs a reachable Moonraker; add `--camera` to exercise the
snapshot tool. See [CONTRIBUTING.md](CONTRIBUTING.md) for the safety invariants and how to add
a tool. The full tool reference is in [docs/tools.md](docs/tools.md).

## License

MIT. See [LICENSE](LICENSE).
