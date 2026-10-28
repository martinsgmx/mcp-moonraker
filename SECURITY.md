# Security policy

`mcp-moonraker` is a bridge between an AI agent and a machine that can heat, move, and
print. Treat it as privileged software. This document describes the threat model, the
controls that exist, and how to deploy it without making a bad situation worse.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it privately to the
maintainer of the repository you obtained this from, with:

- the version and commit,
- the configuration required to reproduce it (redact secrets),
- the exact request or tool call,
- the impact you observed.

You can expect an acknowledgement and a coordinated disclosure timeline. There is no bug
bounty.

## Supported versions

The latest 0.1.x release is supported with security fixes. Older versions may not be.

## Threat model

The server assumes:

- The Moonraker instance and the printer are on a trusted network, or are protected by
  separate authentication.
- The MCP client and the server run under the same user, or the server is reached through
  an authenticated transport.
- The agent may be manipulated by untrusted content (for example a malicious G-code file
  name or print comment) and must not be trusted with unconstrained physical actions.
- A human is available to approve destructive actions when the deployment requires it.

It does **not** assume that the agent is benevolent, that the network is private, or that
the Moonraker API key is safe to print in logs.

### In scope

- Unauthorized tool execution through the MCP transport.
- Bypassing the local policy, approval, or audit layers.
- Secret leakage through tool output, logs, `doctor`, `print-config`, approvals, or errors.
- SSRF through camera URLs returned by Moonraker.
- Path traversal in file tools.
- Resource exhaustion through oversized Moonraker or camera responses.
- Confusing the prepared action with a different action at confirmation time.

### Out of scope

- Physical risks inherent in operating a 3D printer (fire, burns, failed prints). You are
  responsible for hardware safety, smoke detection, and supervision.
- Vulnerabilities in Moonraker, Klipper, the MCP client, or the agent.
- A compromised host account. If an attacker can run commands as your user, they can read
  the approval state and call Moonraker directly.
- Denial of service by someone who can already reach the HTTP port.

## Controls

### Local policy

Every tool declares a capability (`read`, `mutation`, `host_admin`, `raw_gcode`,
`config_write`) and a risk level. The policy engine decides `Allow`, `NeedsApproval`, or
`Deny` before the handler runs. There is a single execution choke point; tools cannot opt
out.

Defaults:

- Raw G-code: denied.
- Host administration (reboot, shutdown, service restart, update): denied.
- Config writes: denied.
- Read-only mode, when enabled, denies every mutation.
- Emergency stop remains available unless explicitly disabled.

See [docs/safety.md](docs/safety.md) for the matrix.

### Double confirmation

High- and critical-risk actions create a file-backed approval record and return a prompt.
Execution requires `moonraker_confirm_action` with:

- `approval_id`,
- the confirmation phrase in `in_band` / `both` mode,
- `acknowledge: true`,
- and, in `out_of_band` / `both` mode, a human running `mcp-moonraker approve <id>`.

The confirmation executes the stored tool and arguments. Caller-supplied actions are never
accepted at this stage. Records are single-use, expire after the configured TTL, and are
locked during consumption so concurrent processes cannot race.

**Limitation:** `in_band` mode is a speed bump, not a security boundary against a
determined autonomous agent. The phrase is returned to the agent by design. If the agent
must not be able to perform destructive actions on its own, use `out_of_band` or `both`.

### Secrets

- `MOONRAKER_API_KEY` and `MCP_MOONRAKER_HTTP_TOKEN` are held in `SecretString`.
- They are skipped during serialization; `print-config` and `doctor` never print them.
- Tool output and audit records pass through recursive, conservative redaction.
- `moonraker_config` redacts sensitive keys rather than exposing raw configuration.
- Logs go to stderr (or an audit file), never stdout, so a stray line cannot corrupt the
  stdio protocol or leak into an MCP response.

Redaction is key-based. Do not put secrets in free-form values such as printer names or
file names, and do not extend the redaction allow-list without reviewing every caller.

### Transport

- **stdio** is the default. It inherits process privileges and has no network surface.
- **HTTP** is streamable HTTP with SSE compatibility. A static bearer token is required
  when binding to a non-loopback address; startup fails otherwise. Loopback without a
  token is permitted for local clients.
- DNS-rebinding protection is enabled; add explicit `allowed_hosts` / `allowed_origins`
  for reverse proxies.
- Request bodies are capped at 4 MiB. Moonraker JSON responses and camera frames are
  capped separately.
- The `/health` endpoint is intentionally unauthenticated and returns only liveness.

The HTTP server does not terminate TLS. Use a reverse proxy that does, and do not expose
plain HTTP beyond a trusted network.

### Camera URLs

Camera URLs come from Moonraker's webcam list, so the trust boundary is the Moonraker
instance. Additional guards:

- only `http` and `https`,
- cloud metadata endpoints are always blocked,
- optional host allow-list,
- optional rejection of private/LAN destinations,
- byte cap, timeout, and an image content-type or magic-byte check.

An attacker who can modify Moonraker's webcam configuration can make the server fetch a
URL of their choosing, subject to those guards. That is equivalent to controlling the
printer's configuration and is outside the MCP server's trust boundary.

### Files and paths

File operations are restricted to Moonraker's `gcodes` root, and relative paths are
validated against traversal. Configuration uploads and writes are not implemented.

### Audit

Every tool call is audited with tool, risk, capability, approval id, redacted arguments,
outcome, duration, and error. Configure `logging.audit_log` for a persistent JSON-lines
file; otherwise records go to stderr at the `mcp_moonraker::audit` target. Audit files are
created with restrictive permissions and should be rotated by your logging stack.

## Deployment checklist

- [ ] Keep stdio for local agents.
- [ ] Set `MCP_MOONRAKER_HTTP_TOKEN` for HTTP and put TLS in front.
- [ ] Bind HTTP to `127.0.0.1` or a private interface.
- [ ] Use `out_of_band` or `both` approval mode for autonomous or remote agents.
- [ ] Leave raw G-code and host administration disabled.
- [ ] Keep temperature limits at or below the printer's safe values.
- [ ] Run as an unprivileged user that needs no more access than the printer.
- [ ] Protect the state directory; approval phrases are stored there.
- [ ] Back up and monitor the audit log.
- [ ] Keep Moonraker, Klipper, and this server updated.
- [ ] Test emergency stop and confirm the printer's physical safety controls work.

## Known limitations

- The static bearer token has no scopes, rotation, or revocation. Rotate it by restarting
  the server with a new value.
- `in_band` approval can be self-approved by an agent. It prevents accidents, not a
  malicious agent.
- Camera snapshot downscaling is not implemented. Large frames are returned as-is within
  the configured byte cap.
- The audit log is append-only from the process's perspective; it does not provide tamper
  evidence.
- Windows support is not tested. File permissions are enforced on Unix; on other
  platforms the state directory relies on the OS defaults.
