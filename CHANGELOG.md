# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses semantic
versioning.

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
