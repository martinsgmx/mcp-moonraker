# Contributing

Thanks for improving `mcp-moonraker`. This project touches hardware that can heat and move,
so safety review matters more than feature volume.

## Development setup

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

A live Moonraker is only needed for integration-style checks:

```bash
mcp-moonraker doctor
python3 scripts/smoke_mcp.py target/debug/mcp-moonraker --camera
```

The smoke test is read-only. Do not add destructive calls to it. If you need to test an
approval flow, use a mock Moonraker (`wiremock` is already a dev-dependency) or a printer
you own, and keep the action harmless (for example a nonexistent file that fails
validation before any motion).

## Repository layout

```text
src/config.rs          layered config, validation, secret-free views
src/moonraker/         typed JSON-RPC + REST client with response limits
src/camera.rs          webcam discovery, snapshot validation, SSRF guards
src/safety/policy.rs   capability/risk policy and temperature limits
src/safety/approval.rs file-backed single-use approvals
src/tools/             tool registry, schemas, and handlers
src/server.rs          MCP tools/resources/prompts handler
src/transport.rs       stdio and streamable HTTP
src/main.rs            CLI, doctor, init, approvals
```

## Safety invariants

Changes must preserve these properties:

1. Every tool has a `ToolSpec` with an explicit `Capability` and `Risk`.
2. Every execution goes through `ToolRegistry::call` or `execute_approved`; no handler is
   called directly from the MCP server.
3. High/critical `mutation` tools return an approval prompt instead of acting.
4. `moonraker_confirm_action` executes the stored tool and arguments only. Never accept a
   fresh action at confirmation time.
5. Approvals are single-use and consumed before execution, even if execution fails.
6. Emergency stop remains available in read-only mode unless explicitly disabled.
7. Raw G-code, host administration, and config writes are denied by default.
8. Secrets never enter `Config` serialization, logs, tool output, audit records, or
   `doctor` output.
9. New paths are relative, validated, and cannot escape their Moonraker root.
10. New network fetches have a timeout, a size cap, and a clear trust boundary.

If a change weakens one of these, it needs an explicit design discussion in the pull
request, not just a passing test.

## Adding a tool

1. Add a `ToolSpec` in the appropriate `src/tools/*.rs` module.
2. Choose the lowest capability and risk that accurately describe the effect. A read that
   returns secrets is not a read; it is a policy problem.
3. Write a JSON Schema with `schema_object` and keep `allowed_args` in sync. Unknown
   arguments are rejected.
4. Add a human-readable summary for approval prompts.
5. Validate all arguments and check live printer state before sending G-code.
6. Add the tool to `docs/tools.md` and run `mcp-moonraker tools` to confirm its gate.
7. Add unit tests for validation and policy behavior. Prefer a mock client over a live
   printer.
8. Update the README tool count and any category summary if the number changes.

## Code style

- `cargo fmt` is mandatory.
- Clippy runs with `-D warnings` and the pedantic group. Prefer a narrowly justified
  `#[allow(...)]` with a comment over weakening the workspace lint configuration.
- Keep handler futures `Send` and tied to the `ToolContext` lifetime; the registry relies on
  the `ToolFn` signature.
- Error messages should tell an agent what to do next without leaking secrets.
- Tracing goes to stderr only. Never print to stdout outside the MCP protocol or a CLI
  command's intended output.

## Tests

- Unit tests live next to the code.
- Use `tempfile` for state directories.
- Use `wiremock` for Moonraker HTTP behavior.
- Test policy denials and approval expiry, not just the happy path.
- A test that needs a real printer should be ignored by default and documented.

## Documentation

Update the relevant file when behavior changes:

- `README.md` — user-facing overview and quick start
- `docs/configuration.md` — every key and environment variable
- `docs/tools.md` — tool list, arguments, and risk
- `docs/safety.md` — policy and approval behavior
- `docs/deployment.md` — transports, service units, Docker, proxies
- `docs/setup-and-security.md` — network topologies, Moonraker `[authorization]`, connectivity troubleshooting
- `SECURITY.md` — threat model or control changes
- `CHANGELOG.md` — user-visible changes

## Pull requests

- Keep changes focused; separate refactors from behavior changes.
- Describe the safety impact and how you tested it.
- Include the output of `cargo test` and `cargo clippy` when relevant.
- Do not commit secrets, approval state, audit logs, or a filled-in `.env`.
- By contributing, you agree that your contribution is licensed under the MIT license.
