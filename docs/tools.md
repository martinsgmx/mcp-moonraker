# Tools, resources, and prompts

All tool results are MCP `content` blocks. Most tools return pretty-printed JSON in a text
block; `moonraker_camera_snapshot` returns a text metadata block plus an `image` block.
Errors set `isError: true` and contain a human-readable explanation. Tool names are stable;
argument names are listed below and unknown arguments are rejected.

Risk and gate are shown for the default configuration. See [safety.md](safety.md) for the
policy rules.

## Status and discovery

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_verify_connection` | read | — | Probe Moonraker: `connected`, latency, Moonraker/Klipper versions; on failure a classified diagnosis with next steps (never an error result) |
| `moonraker_server_info` | read | — | Moonraker version, components, Klippy connection state |
| `moonraker_printer_status` | read | — | Curated snapshot: print state/progress, position, homing, speeds, factors, fan, heater temperatures |
| `moonraker_printer_objects` | read | `objects`, `fields` | List loaded objects, or query named objects with optional field lists |
| `moonraker_temperatures` | read | — | Current/target temperatures for heaters, bed, sensors, temperature fans |
| `moonraker_temperature_history` | read | `sensors`, `limit` | Recent samples from Moonraker's temperature store |
| `moonraker_endstops` | read | — | Endstop states (`open` / `TRIGGERED`) |
| `moonraker_gcode_history` | read | `count` | Recent G-code console output |
| `moonraker_system_info` | read | — | Host CPU, memory, distribution, services, network, processes |
| `moonraker_config` | read | `section` | Moonraker/Klipper configuration with secrets redacted |
| `moonraker_update_status` | read | — | Update manager status and available versions |
| `moonraker_print_history` | read | `limit`, `start` | Completed/failed jobs from Moonraker history |

`moonraker_printer_objects` supports two forms. With no `objects`, it lists loaded object
names. With `objects`, it queries only those objects; `fields` optionally limits the
returned properties. This is the best way to inspect something not covered by a curated
tool without dumping the entire object tree.

## Camera

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_cameras` | read | — | Webcams from Moonraker, including enabled state and snapshot URL |
| `moonraker_camera_snapshot` | read | `camera`, `max_bytes` | Fetch the current frame as an MCP image |

The camera name may be omitted when exactly one webcam is enabled. With multiple enabled
cameras, pass a name from `moonraker_cameras`. `max_bytes` can lower the per-request cap but
cannot exceed `camera.max_bytes`. Supported image types are validated by content type or
magic bytes; HTML error pages are rejected.

## Heaters and fans

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_set_heater` | medium | `heater`, `temperature` | Set a heater/bed target, capped by policy; `0` turns it off |
| `moonraker_turn_off_heaters` | medium | — | Turn off every heater and the bed |
| `moonraker_set_fan` | low | `fan`, `percent` | Part-cooling fan or `fan_generic`, 0–100% |
| `moonraker_set_speed_factor` | low | `percent` | M220 speed factor, 10–200% |
| `moonraker_set_flow_factor` | low | `percent` | M221 flow factor, 50–150% |

Heater names must be loaded Klipper objects. Bed detection is by name (`heater_bed` and
similar), so a heated chamber is capped by the hotend limit unless it is named as a bed.
These tools do not require approval, but they are denied in read-only mode and when
mutations are disabled.

## Motion

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_home_axes` | medium | `axes` | G28 for the selected axes; defaults to all |
| `moonraker_move_axis` | medium | `axis`, `distance`, `speed` | Relative X/Y jog, or upward Z only |

`axis` is `x`, `y`, or `z`. Downward Z movement is rejected by design. The axis must be
homed, and coordinate state is saved/restored around the move. Homing is a physical move;
make sure the bed is clear.

## Files

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_files_list` | read | `root`, `path`, `limit` | List files/directories under a Moonraker root (default `gcodes`) |
| `moonraker_file_metadata` | read | `filename`, `include_checksum` | Slicer metadata, normalized `summary`, `warnings`, optional SHA-256 |
| `moonraker_file_verify` | read | `filename`, `expected_sha256`, `expected_size` | Download a file and verify size/SHA-256 against expected values |
| `moonraker_file_upload` | medium / high | `filename`, `data_base64`, `root`, `overwrite` | Upload base64 G-code to `gcodes`; approval required when overwriting |
| `moonraker_file_delete` | high | `filename` | Delete a file from `gcodes` (approval required) |

Paths are relative to the root and traversal outside it is rejected. Configuration files
cannot be deleted. Uploads are confined to `gcodes`, accept `.gcode`, `.g`, `.gco`, and `.nc`,
and never start a print. By default an existing file is not replaced; set `overwrite=true`
to prepare an approval for the replacement. The decoded payload is capped by
`moonraker.max_upload_bytes`, and the SHA-256 sent with the upload is verified by Moonraker.

`moonraker_file_verify` is read-only: it downloads the file, hashes it locally, compares any
expected values, and returns `verified`, per-field `checks`, metadata `warnings`, and the
actual `sha256`/`size_bytes`. Use it after an upload or a manual transfer to confirm integrity.
`moonraker_file_metadata({filename, include_checksum:true})` is a lighter alternative when a
single hash is enough.

## Print control

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_print_preflight` | read | `filename` | Read-only readiness report with blocking reasons |
| `moonraker_print_start` | high | `filename` | Start a G-code file (approval required) |
| `moonraker_print_pause` | medium | — | Pause the active print |
| `moonraker_print_resume` | medium | — | Resume a paused print |
| `moonraker_print_cancel` | high | — | Cancel the print (approval required) |

`moonraker_print_preflight` consolidates the checks an agent would otherwise perform one by
one: Klippy state, active job, job queue, temperatures, file existence and metadata, and
local policy caps. It returns a `checks` array (`green`/`yellow`/`red`, `blocking`, `detail`,
`evidence`), `blocking_reasons`, `warnings`, and `ready_to_prepare_start`. It is read-only and
never starts a print. Call it before `moonraker_print_start`.

Print start validates that the file exists under `gcodes` when the approved action
executes; Moonraker itself enforces whether the printer is ready to start. Pause/resume/
cancel validate the current print state so they do not silently no-op. Cancel is
irreversible and leaves an incomplete part. A failed execution still consumes the
approval; prepare the action again after fixing the problem.

## Job queue

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_job_queue_status` | read | — | Queue state and jobs |
| `moonraker_job_queue_add` | low | `filenames` | Append files to the queue |
| `moonraker_job_queue_start` | medium | — | Start/resume the queue |
| `moonraker_job_queue_pause` | medium | — | Pause the queue after the current job |
| `moonraker_job_queue_jump` | medium | `job_id` | Move a job to the front |
| `moonraker_job_queue_remove` | high | `job_ids`, `all` | Remove jobs or clear the queue (approval required) |

`all=true` clears the whole queue. It is explicit so a missing `job_ids` does not
accidentally remove everything.

## Configurations and macros

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_list_macros` | read | — | Loaded `gcode_macro` names with their defined variables |
| `moonraker_run_macro` | high | `name`, `params` | Run a loaded `gcode_macro` (approval required) |

Macros may heat, move, or do anything the macro author wrote, so execution is always
approval-gated. `params` is an optional object of key/value pairs (strings, numbers,
booleans) rendered as `NAME KEY=VALUE`; string values are G-code quoted, booleans become
`1`/`0`, and at most 16 parameters are accepted. Prefer a dedicated structured tool when
one exists.

## Restarts and host administration

| Tool | Risk | Arguments | Default |
| --- | --- | --- | --- |
| `moonraker_klipper_restart` | high | — | Approval required |
| `moonraker_firmware_restart` | high | — | Approval required |
| `moonraker_service_restart` | critical | `service` | Disabled; approval when enabled |
| `moonraker_host_reboot` | critical | — | Disabled; approval when enabled |
| `moonraker_host_shutdown` | critical | — | Disabled; approval when enabled |
| `moonraker_update_refresh` | critical | `name` | Disabled; approval when enabled |
| `moonraker_update_upgrade` | critical | `name` | Disabled; approval when enabled |

A Klipper host restart reloads configuration and restarts Klippy without resetting MCUs. A
firmware restart resets Klippy and the MCUs. Both fail an active print. Host administration
is never enabled by default; enabling it exposes the printer's host, not just Klipper.

`moonraker_update_refresh` and `moonraker_update_upgrade` require a component name. Upgrade
accepts exactly one component per call so a typo cannot upgrade everything.

## Raw G-code

| Tool | Risk | Arguments | Default |
| --- | --- | --- | --- |
| `moonraker_raw_gcode` | critical | `script` | Disabled; approval required when enabled |

This bypasses every structured validation. It exists for advanced maintenance, not for
normal agent use. Enabling it requires `policy.allow_raw_gcode = true` (or
`MCP_MOONRAKER_ALLOW_RAW_GCODE=true`); even then each call needs approval.

## Safety and approval introspection

| Tool | Risk | Arguments | What it does |
| --- | --- | --- | --- |
| `moonraker_policy_info` | read | — | Effective policy and approval settings (no secrets) |
| `moonraker_pending_approvals` | read | — | Open approvals, including recovered ids after restart |
| `moonraker_approval_detail` | read | `approval_id` | Full non-secret detail for one approval (any status) |
| `moonraker_cancel_approval` | read | `approval_id` | Cancel an approval permanently |
| `moonraker_confirm_action` | high | `approval_id`, `confirmation_phrase`, `acknowledge` | Execute the prepared action |
| `moonraker_emergency_stop` | critical | — | Immediate Klipper emergency stop |

`moonraker_confirm_action` is the only tool that executes a stored approval. It never
accepts a new action. Both approval-listing tools report `target`, `risk`, `expires_at`, and
a human-readable `summary`; `moonraker_approval_detail` also works for consumed, denied, and
expired records.

When confirmation cannot be completed, `moonraker_confirm_action` returns a structured
error (`isError: true`) with `reason`, `message`, `approval_id`, `tool`, `expires_at` /
`expired_at`, `requires_reprepare` / `reprepare_required`, `no_action_executed`, and
`next_action`. Reasons are
`acknowledgement_required`, `human_approval_required`, `phrase_mismatch` (retryable with the
same id), and `not_found`, `invalid_id`, `not_open`, `already_consumed`, `denied`, `expired`
(re-prepare required). `moonraker_emergency_stop` bypasses read-only mode and approvals; it
is the one action that must always be available.

## Resources

Resources are read-only shortcuts to the same data. They are useful for clients that
support resource attachments but not tool calls.

| URI | Content |
| --- | --- |
| `moonraker://status` | Printer status snapshot |
| `moonraker://temperatures` | Heater and sensor temperatures |
| `moonraker://endstops` | Endstop states |
| `moonraker://cameras` | Configured webcams |
| `moonraker://config` | Redacted Moonraker/Klipper configuration |
| `moonraker://system` | Host system information |
| `moonraker://policy` | Local safety policy |
| `moonraker://approvals` | Pending approvals |

Resources still go through the tool registry, so policy and audit apply.

## Prompts

| Prompt | Arguments | Purpose |
| --- | --- | --- |
| `printer_health_check` | — | Structured check of connectivity, state, temperatures, and recent errors |
| `print_failure_triage` | `symptom` | Triage flow for a failed or failing print |

Prompts are advisory text; they do not execute tools. The agent still has to call the
appropriate read tools.

## Argument conventions

- Names are snake_case and match the table exactly.
- Unknown arguments are rejected rather than ignored.
- Numbers must be finite; strings are trimmed where appropriate.
- File paths are relative, use `/`, and may not escape the root.
- Booleans are real JSON booleans (`true`), not strings.
- Optional arguments may be omitted or set to `null`.
- `data_base64` uses the standard alphabet; embedded whitespace is ignored and payloads are
  size-capped. Payloads are redacted in audit logs.
