# Safety model

This server is designed to let an agent be useful without giving it an unfiltered G-code
console. The safety model has four independent layers:

1. **Local policy** — capability and risk decide whether a tool may run at all.
2. **Double confirmation** — high/critical actions require a prepared, single-use approval.
3. **Structured tools** — validate arguments against live printer state and hard limits.
4. **Audit and observability** — every call is recorded with its outcome.

No layer relies on the agent behaving well.

## Capabilities and risk

Every tool declares one capability and one risk level.

| Capability | Meaning | Default decision |
| --- | --- | --- |
| `read` | No printer-side effect | Allow |
| `mutation` | Changes state; reversible or bounded | Allow below `high`, approval at `high`/`critical` |
| `host_admin` | Reboot, shutdown, service restart, updates | **Deny** |
| `raw_gcode` | Arbitrary Klipper G-code | **Deny** |
| `config_write` | Configuration file writes | **Deny** (not implemented in v0.1) |

| Risk | Examples |
| --- | --- |
| `read` | Status, temperatures, camera, files list, history |
| `low` | Fan speed, speed/flow factor, queue add |
| `medium` | Heater target, home, jog, pause/resume, queue start |
| `high` | Print start/cancel, file delete, Klipper/firmware restart, macro, queue remove |
| `critical` | Raw G-code, reboot, shutdown, service restart, updates, emergency stop |

`read_only` denies every non-read capability, including low-risk mutations. It does **not**
disable emergency stop. `allow_mutations=false` denies mutations while leaving reads
available. Both switches are checked before the capability-specific rules.

## Default approval-gated actions

With default configuration, these seven tools prepare an approval instead of acting:

- `moonraker_print_start`
- `moonraker_print_cancel`
- `moonraker_file_delete`
- `moonraker_klipper_restart`
- `moonraker_firmware_restart`
- `moonraker_run_macro`
- `moonraker_job_queue_remove`

`moonraker_raw_gcode`, `moonraker_host_reboot`, `moonraker_host_shutdown`,
`moonraker_service_restart`, `moonraker_update_refresh`, and
`moonraker_update_upgrade` are denied outright until their capability is enabled; then they
also require approval.

`moonraker_emergency_stop` is never approval-gated and works in read-only mode.

Run `mcp-moonraker tools` or call `moonraker_policy_info` for the live list under your
configuration.

## The approval flow

```text
agent                         server                         human (out-of-band)
  |                              |                                  |
  |-- moonraker_print_start ---->|                                  |
  |                              | policy: NeedsApproval            |
  |                              | create apr_... (store, 0600)     |
  |<-- approval_id, phrase ------|                                  |
  |                              |                                  |
  |                              |<---- mcp-moonraker approve apr_..|
  |-- moonraker_confirm_action ->|                                  |
  |   id + phrase + ack=true     | verify, lock, mark consumed      |
  |                              | execute stored tool + args       |
  |<-- result + consumption -----|                                  |
```

Properties:

- The action is defined at prepare time. `moonraker_confirm_action` does not accept a new
  tool name or new arguments.
- Confirmation requires `acknowledge: true`. Omitting it returns an error and leaves the
  approval open.
- In `in_band` and `both` modes the phrase is compared in constant time.
- In `out_of_band` and `both` modes a human must set `human_approved_at` through the CLI.
- Records are single-use. A consumed approval cannot be retried, even if execution failed.
- Expired, denied, and consumed records are rejected with distinct messages.
- The policy is re-evaluated at execution time. If an operator tightens the policy between
  prepare and confirm, the action is refused (and the approval is still consumed).
- Preparation validates argument shape only. Target existence and live printer-state checks
  run when the approved action executes. A failed execution still consumes the approval.
- Per-record file locks make `approve`/`deny`/`confirm` safe across processes.

Approval records contain the tool name, arguments, summary, risk, phrase, and timestamps.
They may contain file names or macro names. Protect the state directory accordingly.

### Choosing a mode

- `in_band` stops accidental agent actions but an autonomous agent can complete the flow
  itself. It is the default because most users run a trusted local agent.
- `out_of_band` is the right choice when the agent runs unattended or the MCP server is
  reachable by more than one person.
- `both` is appropriate for a shared or remote deployment.

If you want a hard read-only server, set `read_only=true`; approvals do not override it.

## Argument validation

Structured tools validate before sending anything to Klipper:

- **Heaters** — the heater must be a loaded object; targets are finite, non-negative, and
  capped by `max_hotend_temp` or `max_bed_temp`. A target of `0` turns the heater off.
- **Fans** — only the part-cooling fan and `fan_generic` objects; automatic fans
  (`heater_fan`, `temperature_fan`) are rejected. Percent is 0–100.
- **Speed/flow** — speed 10–200%, flow 50–150%.
- **Motion** — X and Y may jog relative distances; Z may only move **upward**. A downward
  Z move is rejected because an unhomed or miscalibrated printer can crash the nozzle into
  the bed. The move saves and restores G-code coordinate state. The axis must be homed.
- **Homing** — triggers a physical move; ensure the bed is clear.
- **Print start** — the file must exist under `gcodes`; the printer must be in a state that
  permits starting.
- **Print pause/resume/cancel** — validated against the current print state.
- **Files** — paths are relative to the `gcodes` root and traversal is rejected. Deleting
  configuration files is not supported.
- **Queue** — job ids and file names are validated; `all=true` is explicit.
- **Updates** — a component name is mandatory, so a missing argument cannot trigger a
  blanket upgrade.
- **Raw G-code** — no semantic validation; this is why it is disabled by default and
  approval-gated when enabled.

## Temperature and physical safety

The server enforces software limits, but they are not a substitute for hardware safety:

- Keep `max_hotend_temp` and `max_bed_temp` at or below the manufacturer's limits. The
  defaults (300 °C / 120 °C) are common but not universal.
- Klipper's own `max_temp` and `min_extrude_temp` checks still apply; do not disable them.
- A heater target is not a guarantee that the heater is working. Watch temperatures during
  the first autonomous session.
- Ensure thermal runaway protection, fuses, and a smoke detector are in place.
- Verify that an emergency stop actually works. The MCP tool calls Klipper's emergency stop;
  a physical power switch is still the ultimate control.

## Audit

Each call writes an event with:

```json
{
  "timestamp": 1793220000,
  "event": "tool_call",
  "tool": "moonraker_print_start",
  "risk": "high",
  "capability": "mutation",
  "approval_id": "apr_...",
  "outcome": "executed",
  "duration_ms": 42,
  "args": {"filename": "benchy.gcode"},
  "error": null
}
```

Outcomes include `prepared`, `allowed`, `denied`, `approval_required`, `confirmed`,
`executed`, and `failed`. Arguments are redacted and truncated. Set
`logging.audit_log` to persist events; otherwise they go to stderr under the
`mcp_moonraker::audit` target.

Audit records are best-effort: a full disk can prevent a write, but it cannot cause an
action to bypass policy. Treat the MCP server's stderr and your process supervisor's logs
as part of the audit trail.

## What this model does not protect against

- A malicious agent in `in_band` mode. Use `out_of_band` or `both`.
- A compromised user account or host. Approval files are readable by that user.
- A compromised Moonraker instance. It can lie about printer state and camera URLs.
- Direct access to Moonraker's API or Klipper's console. This server is not a firewall.
- Physical failure, fire, or injury. Hardware safety is your responsibility.
