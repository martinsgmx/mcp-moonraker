# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses semantic
versioning.

## [Unreleased]

### Added

- `moonraker.script_timeout_secs` / `MOONRAKER_SCRIPT_TIMEOUT_SECS` (default
  `120`, range 1–600): a dedicated timeout for `printer.gcode.script` calls.
  Klipper only answers those requests after the script has finished executing,
  so `moonraker_home_axes` (G28), `moonraker_move_axis`, `moonraker_run_macro`,
  and heater/fan scripts previously hit the generic 15-second request timeout
  and reported an error even though the action completed on the printer. Every
  G-code script call now waits up to the script timeout and only returns once
  the action has really ended.

- `moonraker_verify_connection` tool: a read-only first-call probe that reports
  `connected`, round-trip latency in milliseconds, Moonraker/Klipper versions,
  Klippy state, and — when Moonraker is unreachable or rejects credentials — a
  classified diagnosis plus concrete next steps, returned as a normal tool
  result instead of an error. The server's advertised `instructions` now tell
  the agent to call the verifier when any tool reports a transport, timeout, or
  unauthorized failure, and to stop chaining printer tools until it reports
  `connected: true`.
- `moonraker_list_macros` tool: lists loaded `gcode_macro` objects with their defined
  variables, so agents can pick a valid macro name before `moonraker_run_macro`
  instead of error-probing the generic object list.
- `moonraker_run_macro` now accepts optional `params` (object of up to 16
  string/number/boolean pairs). Parameters are rendered into the G-code script as
  `KEY=VALUE` (numbers unquoted, strings G-code quoted, booleans as `1`/`0`) and
  appear in the approval summary, so `PRINT_START BED=110 NOZZLE=240` works
  through the existing approval flow.
- `docs/setup-and-security.md`: a real-world setup guide covering URL selection for every
  network topology (same host, LAN, reverse-proxied HTTPS, remote/VPN), Moonraker
  `[authorization]` configuration (API key retrieval and rotation, `trusted_clients`,
  why `force_logins` blocks non-interactive clients, why `cors_domains` is irrelevant),
  and a troubleshooting matrix for requests that never reach Moonraker.

### Changed

- Transport failures surfaced through `public_message` (tool errors, `doctor`) now
  classify the reqwest cause — connection refused, DNS resolution, unreachable route,
  timeout, TLS certificate — and append one actionable hint per class.
- Moonraker 401 responses now explain the fix: set `MOONRAKER_API_KEY` from
  `/access/api_key`, rotate stale keys, and avoid `force_logins`.

## [0.1.0] - 2026-10-28

Initial release.

### Added

- MCP server for Moonraker/Klipper over stdio and streamable HTTP (SSE-compatible).
- 47 tools: status, discovery, cameras, heaters/fans, motion, files, print control, job
  queue, macros, restarts, host administration, and safety introspection.
- MCP resources for status, temperatures, endstops, cameras, config, system information,
  policy, and pending approvals.
- Prompts for a printer health check and print failure triage.
- Capability/risk policy engine with safe defaults: raw G-code, host administration, and
  config writes disabled; emergency stop always available.
- File-backed, single-use approvals with TTL, per-record locking, in-band,
  out-of-band, and combined modes.
- Emergency stop bypasses read-only mode and approvals.
- Temperature caps, heater/object validation, motion guardrails (no downward Z), and
  traversal-safe file paths.
- Camera snapshots as MCP image blocks, using Moonraker's webcam list with SSRF and
  metadata-host guards.
- Secret-free configuration views, recursive redaction, and JSON-lines audit logging.
- CLI: `serve`, `doctor`, `approve`, `deny`, `pending`, `print-config`, `tools`, `init`.
- Layered TOML/environment/flag configuration with validation and a starter config.
- Docker image, CI workflow, agent examples, and a read-only MCP smoke test.

[0.1.0]: https://github.com/martinsgmx/mcp-moonraker/releases/tag/v0.1.0
