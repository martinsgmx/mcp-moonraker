# Configuration

Configuration is layered; later sources win:

1. Built-in defaults
2. TOML file (`--config FILE`, `MCP_MOONRAKER_CONFIG`, or `./mcp-moonraker.toml` only if
   explicitly passed — there is no implicit file discovery)
3. Environment variables
4. CLI flags

Unknown TOML keys are rejected so a typo cannot silently fall back to a less safe default.
Relative state and audit paths are resolved from the process working directory; `~/` is
expanded.

Generate a commented starter file with `mcp-moonraker init --output mcp-moonraker.toml`.
Print the resolved values (secrets omitted) with `mcp-moonraker print-config`.

## moonraker

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `moonraker.url` | `MOONRAKER_URL` | `http://127.0.0.1:7125` | `http`/`https`; may include a reverse-proxy path prefix; no query or fragment |
| `moonraker.api_key` | `MOONRAKER_API_KEY` | unset | Sent as `X-Api-Key`; prefer the environment. Get the value from `curl http://127.0.0.1:7125/access/api_key` on the printer host; see [setup-and-security.md](setup-and-security.md) |
| `moonraker.timeout_secs` | `MOONRAKER_TIMEOUT_SECS` | `15` | 1–300 seconds |
| `moonraker.script_timeout_secs` | `MOONRAKER_SCRIPT_TIMEOUT_SECS` | `120` | 1–600 seconds; used for `printer.gcode.script` calls, which return only after Klipper finishes the script (homing, long macros, `TEMPERATURE_WAIT`) |
| `moonraker.tls_verify` | `MOONRAKER_TLS_VERIFY` | `true` | Disable only for a trusted self-signed host |
| `moonraker.max_response_bytes` | `MOONRAKER_MAX_RESPONSE_BYTES` | `8388608` | 1 KiB–64 MiB |

The API key is only forwarded to camera URLs on the same origin as Moonraker. Camera URLs
on another host get no Moonraker credentials.

## server

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `server.transport` | `MCP_MOONRAKER_TRANSPORT` | `stdio` | `stdio` or `http` |
| `server.http.host` | `MCP_MOONRAKER_HTTP_HOST` | `127.0.0.1` | Bind address |
| `server.http.port` | `MCP_MOONRAKER_HTTP_PORT` | `8765` | 1–65535 |
| `server.http.path` | `MCP_MOONRAKER_HTTP_PATH` | `/mcp` | Streamable HTTP endpoint |
| `server.http.token` | `MCP_MOONRAKER_HTTP_TOKEN` | unset | Static bearer token; required off-loopback |
| `server.http.allowed_hosts` | `MCP_MOONRAKER_HTTP_ALLOWED_HOSTS` | `[]` | Comma-separated extra `Host` values for DNS-rebinding protection |
| `server.http.allowed_origins` | `MCP_MOONRAKER_HTTP_ALLOWED_ORIGINS` | `[]` | Comma-separated extra browser origins |
| `server.http.allow_unauthenticated` | `MCP_MOONRAKER_HTTP_ALLOW_UNAUTHENTICATED` | `false` | Explicit opt-out of the off-loopback token requirement |

The SSE compatibility endpoints are fixed at `/sse` and `/messages`; the health endpoint
is `/health` and is unauthenticated.

## policy

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `policy.read_only` | `MCP_MOONRAKER_READ_ONLY` | `false` | Denies every mutation; emergency stop still works |
| `policy.allow_mutations` | `MCP_MOONRAKER_ALLOW_MUTATIONS` | `true` | Master switch for reversible mutations |
| `policy.allow_raw_gcode` | `MCP_MOONRAKER_ALLOW_RAW_GCODE` | `false` | Enables `moonraker_raw_gcode` (still approval-gated) |
| `policy.allow_host_admin` | `MCP_MOONRAKER_ALLOW_HOST_ADMIN` | `false` | Enables reboot/shutdown/service/update tools |
| `policy.allow_config_write` | `MCP_MOONRAKER_ALLOW_CONFIG_WRITE` | `false` | Reserved; no write tools ship in v0.1 |
| `policy.max_hotend_temp` | `MCP_MOONRAKER_MAX_HOTEND_TEMP` | `300.0` | 0–500 °C hard cap |
| `policy.max_bed_temp` | `MCP_MOONRAKER_MAX_BED_TEMP` | `120.0` | 0–200 °C hard cap |
| `policy.disable_emergency_stop` | `MCP_MOONRAKER_DISABLE_EMERGENCY_STOP` | `false` | Removes the only always-allowed action |

`read_only=true` together with `allow_raw_gcode=true` is rejected as contradictory.
`allow_mutations=false` also blocks low-risk actions such as fans and pause/resume.
Temperature limits apply to `moonraker_set_heater`; values above the configured cap are
rejected before any G-code is sent.

## approval

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `approval.mode` | `MCP_MOONRAKER_APPROVAL_MODE` | `in_band` | `in_band`, `out_of_band`, or `both` |
| `approval.ttl_secs` | `MCP_MOONRAKER_APPROVAL_TTL_SECS` | `120` | 5–3600 seconds |
| `approval.state_dir` | `MCP_MOONRAKER_STATE_DIR` | OS state dir | Defaults to `$XDG_STATE_HOME/mcp-moonraker` or `~/.local/state/mcp-moonraker` |

Modes:

| Mode | Phrase returned to agent | Human `approve` required | Use case |
| --- | --- | --- | --- |
| `in_band` | yes | no | Trusted local agent; prevents accidents |
| `out_of_band` | no | yes | Autonomous agent or shared host |
| `both` | yes | yes | Remote exposure and strongest control |

Approval records are stored under `<state_dir>/approvals/`. Directories are created with
`0700` and records with `0600` on Unix. The TTL is evaluated at confirmation time; expired
records cannot be revived. `mcp-moonraker pending` reads the same store and works while the
server is running.

## camera

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `camera.max_bytes` | `MCP_MOONRAKER_CAMERA_MAX_BYTES` | `8388608` | 1 KiB–64 MiB |
| `camera.timeout_secs` | `MCP_MOONRAKER_CAMERA_TIMEOUT_SECS` | `10` | 1–120 seconds |
| `camera.allow_private_hosts` | `MCP_MOONRAKER_CAMERA_ALLOW_PRIVATE_HOSTS` | `true` | Needed for most LAN webcams |
| `camera.allowed_hosts` | `MCP_MOONRAKER_CAMERA_ALLOWED_HOSTS` | `[]` | Optional host allow-list; empty means any host returned by Moonraker |

A tool call may lower `max_bytes` per request but cannot exceed the configured limit. Cloud
metadata endpoints are blocked regardless of settings.

## logging

| TOML key | Environment | Default | Notes |
| --- | --- | --- | --- |
| `logging.level` | `MCP_MOONRAKER_LOG_LEVEL` | `info` | A `tracing` filter such as `mcp_moonraker=debug`; `RUST_LOG` also works |
| `logging.audit_log` | `MCP_MOONRAKER_AUDIT_LOG` | unset | JSON-lines audit file; stderr when unset |

Tracing always writes to stderr. In stdio mode, stdout is reserved exclusively for MCP
messages.

## CLI overrides

`serve` accepts flags that override the file and environment for that process:

```bash
mcp-moonraker serve \
  --url http://printer.local:7125 \
  --transport http \
  --http-host 0.0.0.0 \
  --http-port 8765 \
  --read-only \
  --approval-mode out_of_band \
  --state-dir /var/lib/mcp-moonraker \
  --log-level mcp_moonraker=debug
```

Flags that enable a capability (`--allow-raw-gcode`, `--allow-host-admin`) only turn it on;
there is no flag to turn a file setting off. Prefer the config file for durable policy.

## Environment file

`.env.example` documents the available variables. The binary does **not** read `.env` files
automatically; load them in your process manager, container runtime, or shell. This avoids
surprising precedence and keeps secrets out of implicit file discovery.

## Minimal examples

Local stdio agent:

```toml
[moonraker]
url = "http://127.0.0.1:7125"
```

Remote printer, out-of-band approvals:

```toml
[moonraker]
url = "http://192.168.1.50:7125"

[approval]
mode = "out_of_band"

[policy]
allow_host_admin = false
allow_raw_gcode = false
```

HTTP on a private interface with a token from the environment:

```toml
[server]
transport = "http"

[server.http]
host = "0.0.0.0"
port = 8765
```

Set `MCP_MOONRAKER_HTTP_TOKEN` in the environment. There is no `${VAR}` interpolation in
TOML, so a literal `token = "..."` in the file would be used as the token itself. Keep
secrets in the environment (or a secret manager) and omit `token` from the file.
