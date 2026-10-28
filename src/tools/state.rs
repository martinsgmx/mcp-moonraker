//! Read-only state tools: server/printer info, temperatures, object queries,
//! history, and configuration inspection.
//!
//! Everything here is `Capability::Read` and cannot alter the printer. Outputs
//! are still passed through the audit logger and, for configuration, the
//! redaction layer.

use serde_json::{json, Map, Value};

use crate::{
    error::{Error, Result},
    moonraker::types::{ServerInfo, WebcamList},
    safety::{Capability, Risk},
    tools::{
        args::{validate_object_name, Args},
        empty_schema, json_result, no_summary, object_names, query_objects, schema_object, Gate,
        ToolContext, ToolSpec,
    },
};

/// Read-only tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_server_info",
            title: "Moonraker server and Klipper information",
            description: "Return Moonraker's version/components/connection state and Klipper's \
                          host information. Use this first to confirm the printer is reachable and \
                          whether Klipper is ready, busy, or shut down.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: empty_schema,
            handler: |ctx, _args| Box::pin(server_info(ctx)),
        },
        ToolSpec {
            name: "moonraker_printer_status",
            title: "Printer status snapshot",
            description: "Return a curated snapshot of printer state: print job state and progress, \
                          positions, homing state, speeds and factors, fan speed, idle state, and \
                          main heater temperatures. Prefer this for broad status questions.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: empty_schema,
            handler: |ctx, _args| Box::pin(printer_status(ctx)),
        },
        ToolSpec {
            name: "moonraker_temperatures",
            title: "Heater and sensor temperatures",
            description: "Return current and target temperatures for every heater, heated bed, \
                          temperature sensor, and temperature-controlled fan. Use this for \
                          'what is the nozzle/bed/chamber temperature?' questions.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: empty_schema,
            handler: |ctx, _args| Box::pin(temperatures(ctx)),
        },
        ToolSpec {
            name: "moonraker_temperature_history",
            title: "Temperature history",
            description: "Return recent temperature samples from Moonraker's in-memory temperature \
                          store. Optionally filter by sensor names and limit the number of samples \
                          per sensor (newest kept).",
            allowed_args: &["sensors", "limit"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: temperature_history_schema,
            handler: |ctx, args| Box::pin(temperature_history(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_printer_objects",
            title: "Query Klipper printer objects",
            description: "List loaded Klipper printer objects or query selected objects. Use the \
                          list form to discover objects such as heaters, fans, macros, and sensors; \
                          use the query form with explicit object names and optional field lists to \
                          avoid returning large payloads.",
            allowed_args: &["objects", "fields"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: printer_objects_schema,
            handler: |ctx, args| Box::pin(printer_objects(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_endstops",
            title: "Endstop states",
            description: "Return the current state of every configured endstop (`open` or \
                          `TRIGGERED`). Useful for diagnosing homing failures.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: empty_schema,
            handler: |ctx, _args| Box::pin(endstops(ctx)),
        },
        ToolSpec {
            name: "moonraker_print_history",
            title: "Print history",
            description: "Return completed/failed print jobs from Moonraker's history database, \
                          newest first. Includes status, duration, filament used, and metadata.",
            allowed_args: &["limit", "start"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: history_schema,
            handler: |ctx, args| Box::pin(print_history(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_gcode_history",
            title: "Recent G-code console output",
            description: "Return recent G-code console messages from Moonraker's in-memory store. \
                          Useful for diagnosing command errors and macro output.",
            allowed_args: &["count"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: gcode_history_schema,
            handler: |ctx, args| Box::pin(gcode_history(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_config",
            title: "Moonraker and Klipper configuration",
            description: "Return Moonraker's effective configuration (or one top-level section). \
                          Secrets such as API keys, passwords, and tokens are redacted. This is the \
                          authoritative read path for configuration; writing configuration is not \
                          supported.",
            allowed_args: &["section"],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: config_schema,
            handler: |ctx, args| Box::pin(config(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_system_info",
            title: "Host system information",
            description: "Return host CPU, memory, distribution, service state, network, and \
                          process statistics reported by Moonraker. Useful for diagnosing host \
                          resource pressure and service health.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: empty_schema,
            handler: |ctx, _args| Box::pin(system_info(ctx)),
        },
    ]
}

async fn server_info(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let server: Value = ctx.client.call_value("server.info", None).await?;
    let printer: Value = ctx.client.call_value("printer.info", None).await?;
    let webcams: WebcamList = ctx
        .client
        .call("server.webcams.list", None)
        .await
        .unwrap_or_default();
    let info: ServerInfo = serde_json::from_value(server.clone()).unwrap_or_default();

    Ok(json_result(
        &json!({
            "server": {
                "moonraker_version": info.moonraker_version,
                "api_version": info.api_version_string,
                "klippy_connected": info.klippy_connected,
                "klippy_state": info.klippy_state,
                "components": info.components,
                "failed_components": info.failed_components,
                "warnings": info.warnings,
                "websocket_count": info.websocket_count,
                "registered_directories": info.registered_directories,
            },
            "klipper": crate::safety::redact::redact_value(&printer),
            "webcams": webcams.webcams.iter().map(|camera| json!({
                "name": camera.name,
                "enabled": camera.enabled,
                "snapshot_available": camera.snapshot_url.is_some(),
            })).collect::<Vec<_>>(),
            "raw_server_info": crate::safety::redact::redact_value(&server),
        }),
        None,
    ))
}

async fn printer_status(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let mut objects = Map::new();
    objects.insert(
        "print_stats".into(),
        json!([
            "state",
            "filename",
            "message",
            "print_duration",
            "total_duration",
            "filament_used",
            "info"
        ]),
    );
    objects.insert(
        "virtual_sdcard".into(),
        json!(["progress", "is_active", "file_position", "file_size"]),
    );
    objects.insert(
        "gcode_move".into(),
        json!([
            "position",
            "gcode_position",
            "speed_factor",
            "extrude_factor",
            "speed",
            "homing_origin",
            "absolute_coordinates"
        ]),
    );
    objects.insert(
        "toolhead".into(),
        json!([
            "position",
            "homed_axes",
            "status",
            "max_velocity",
            "max_accel"
        ]),
    );
    objects.insert("extruder".into(), json!(["temperature", "target", "power"]));
    objects.insert(
        "heater_bed".into(),
        json!(["temperature", "target", "power"]),
    );
    objects.insert("fan".into(), json!(["speed", "rpm"]));
    objects.insert("idle_timeout".into(), json!(["state", "printing_time"]));
    objects.insert("display_status".into(), json!(["progress", "message"]));
    objects.insert("pause_resume".into(), json!(["is_paused"]));
    objects.insert(
        "motion_report".into(),
        json!(["live_position", "live_velocity", "live_extruder_velocity"]),
    );
    objects.insert(
        "system_stats".into(),
        json!(["sysload", "cputime", "memavail"]),
    );
    objects.insert(
        "exclude_object".into(),
        json!(["current_object", "excluded_objects"]),
    );

    let value = query_objects(ctx, objects).await?;
    let status = value.get("status").cloned().unwrap_or(Value::Null);

    Ok(json_result(
        &json!({
            "state": status.get("print_stats"),
            "file": status.get("print_stats").and_then(|stats| stats.get("filename")),
            "progress": status.get("virtual_sdcard"),
            "motion": status.get("gcode_move"),
            "toolhead": status.get("toolhead"),
            "temperatures": {
                "extruder": status.get("extruder"),
                "heater_bed": status.get("heater_bed"),
            },
            "fan": status.get("fan"),
            "idle": status.get("idle_timeout"),
            "display": status.get("display_status"),
            "paused": status.get("pause_resume").and_then(|value| value.get("is_paused")),
            "live_motion": status.get("motion_report"),
            "system": status.get("system_stats"),
            "exclude_object": status.get("exclude_object"),
            "eventtime": value.get("eventtime"),
            "raw_status": status,
        }),
        None,
    ))
}

async fn temperatures(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let names = object_names(ctx).await?;
    let mut objects = Map::new();
    for name in &names {
        if is_temperature_object(name) {
            objects.insert(
                name.clone(),
                json!(["temperature", "target", "power", "measured_power"]),
            );
        }
    }

    if objects.is_empty() {
        return Ok(crate::tools::text_result(
            "No temperature objects were found in Klipper's printer object list.",
        ));
    }

    let value = query_objects(ctx, objects).await?;
    let status = value
        .get("status")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut heaters = Vec::new();
    let mut sensors = Vec::new();
    for (name, data) in status {
        let entry = json!({
            "object": name,
            "temperature": data.get("temperature").cloned().unwrap_or(Value::Null),
            "target": data.get("target").cloned().unwrap_or(Value::Null),
            "power": data.get("power").cloned().unwrap_or(Value::Null),
            "measured_power": data.get("measured_power").cloned().unwrap_or(Value::Null),
        });
        if has_target(&name) {
            heaters.push(entry);
        } else {
            sensors.push(entry);
        }
    }
    heaters.sort_by(|a, b| a["object"].as_str().cmp(&b["object"].as_str()));
    sensors.sort_by(|a, b| a["object"].as_str().cmp(&b["object"].as_str()));

    Ok(json_result(
        &json!({
            "heaters": heaters,
            "sensors": sensors,
            "eventtime": value.get("eventtime"),
        }),
        None,
    ))
}

fn is_temperature_object(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "extruder"
        || lower.starts_with("extruder")
        || lower.starts_with("heater_")
        || lower.starts_with("temperature_sensor")
        || lower.starts_with("temperature_fan")
        || lower.starts_with("temperature_host")
        || lower.starts_with("bme280")
        || lower.starts_with("htu21d")
        || lower.starts_with("lm75")
        || lower.starts_with("ds18b20")
}

fn has_target(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("heater_")
        || lower.starts_with("extruder")
        || lower.starts_with("temperature_fan")
}

fn temperature_history_schema() -> Value {
    schema_object(
        json!({
            "sensors": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional sensor object names to include. Omit for all sensors."
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 500,
                "default": 60,
                "description": "Maximum samples per sensor, newest kept."
            }
        }),
        &[],
    )
}

async fn temperature_history(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["sensors", "limit"])?;
    let sensors = args.optional_string_list("sensors")?;
    let limit = args.optional_u64("limit")?.unwrap_or(60).clamp(1, 500) as usize;

    let store: Value = ctx
        .client
        .call_value("server.temperature_store", None)
        .await?;
    let Some(store) = store.as_object() else {
        return Err(Error::InvalidResponse(
            "server.temperature_store did not return an object".to_string(),
        ));
    };

    if let Some(sensors) = &sensors {
        for sensor in sensors {
            if !store.contains_key(sensor) {
                return Err(Error::InvalidArguments(format!(
                    "sensor '{sensor}' is not present in the temperature store; available: {}",
                    store.keys().cloned().collect::<Vec<_>>().join(", ")
                )));
            }
        }
    }

    let mut output = Map::new();
    for (name, samples) in store {
        if sensors
            .as_ref()
            .is_some_and(|wanted| !wanted.iter().any(|sensor| sensor == name))
        {
            continue;
        }
        let trimmed = samples
            .as_array()
            .map(|samples| {
                let start = samples.len().saturating_sub(limit);
                samples[start..].to_vec()
            })
            .unwrap_or_default();
        output.insert(name.clone(), Value::Array(trimmed));
    }

    Ok(json_result(
        &json!({"limit_per_sensor": limit, "sensors": output}),
        None,
    ))
}

fn printer_objects_schema() -> Value {
    schema_object(
        json!({
            "objects": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional object names to query with all fields. Omit to list all loaded object names."
            },
            "fields": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional field names to request for every queried object. Omit for all fields."
            }
        }),
        &[],
    )
}

async fn printer_objects(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["objects", "fields"])?;
    let objects = args.optional_string_list("objects")?;
    let fields = args.optional_string_list("fields")?;

    let names = object_names(ctx).await?;

    let Some(objects) = objects else {
        return Ok(json_result(
            &json!({"count": names.len(), "objects": names}),
            None,
        ));
    };

    if objects.len() > 20 {
        return Err(Error::InvalidArguments(
            "at most 20 objects may be queried at once".to_string(),
        ));
    }
    if let Some(fields) = &fields {
        if fields.len() > 40 {
            return Err(Error::InvalidArguments(
                "at most 40 fields may be requested".to_string(),
            ));
        }
        for field in fields {
            validate_object_name(field, "field")?;
        }
    }

    let mut query = Map::new();
    for name in &objects {
        let name = validate_object_name(name, "object")?;
        if !names.iter().any(|candidate| candidate == &name) {
            return Err(Error::InvalidArguments(format!(
                "object '{name}' is not loaded; use moonraker_printer_objects without \
                 'objects' to list valid names"
            )));
        }
        query.insert(
            name,
            fields.as_ref().map_or(Value::Null, |fields| json!(fields)),
        );
    }

    let value = query_objects(ctx, query).await?;
    Ok(json_result(&value, None))
}

async fn endstops(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let value = ctx
        .client
        .call_value("printer.query_endstops.status", None)
        .await?;
    Ok(json_result(&json!({"endstops": value}), None))
}

fn history_schema() -> Value {
    schema_object(
        json!({
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 200,
                "default": 20,
                "description": "Maximum number of jobs to return."
            },
            "start": {
                "type": "integer",
                "minimum": 0,
                "default": 0,
                "description": "Offset into the history, newest first."
            }
        }),
        &[],
    )
}

async fn print_history(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["limit", "start"])?;
    let limit = args.optional_u64("limit")?.unwrap_or(20).clamp(1, 200);
    let start = args.optional_u64("start")?.unwrap_or(0);
    let value = ctx
        .client
        .call_value(
            "server.history.list",
            Some(json!({"limit": limit, "start": start, "order": "desc"})),
        )
        .await?;
    Ok(json_result(
        &crate::safety::redact::redact_value(&value),
        None,
    ))
}

fn gcode_history_schema() -> Value {
    schema_object(
        json!({
            "count": {
                "type": "integer",
                "minimum": 1,
                "maximum": 500,
                "default": 50,
                "description": "Number of console messages to return, newest last."
            }
        }),
        &[],
    )
}

async fn gcode_history(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["count"])?;
    let count = args.optional_u64("count")?.unwrap_or(50).clamp(1, 500);
    let value = ctx
        .client
        .call_value("server.gcode_store", Some(json!({"count": count})))
        .await?;
    Ok(json_result(
        &crate::safety::redact::redact_value(&value),
        None,
    ))
}

fn config_schema() -> Value {
    schema_object(
        json!({
            "section": {
                "type": "string",
                "description": "Optional top-level Moonraker config section, for example 'server', 'authorization', or 'webcam'."
            }
        }),
        &[],
    )
}

async fn config(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["section"])?;
    let section = args.optional_string("section")?;
    let value: Value = ctx.client.call_value("server.config", None).await?;
    let config = value.get("config").cloned().unwrap_or(Value::Null);
    let redacted = crate::safety::redact::redact_value(&config);

    if let Some(section) = section {
        let selected = redacted.get(&section).cloned().ok_or_else(|| {
            let available = redacted
                .as_object()
                .map(|map| map.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            Error::InvalidArguments(format!(
                "config section '{section}' not found; available sections: {available}"
            ))
        })?;
        return Ok(json_result(
            &json!({"section": section, "config": selected}),
            Some("Secrets are redacted."),
        ));
    }

    Ok(json_result(
        &json!({
            "config": redacted,
            "files": crate::safety::redact::redact_value(
                &value.get("files").cloned().unwrap_or(Value::Null)
            ),
        }),
        Some("Secrets are redacted."),
    ))
}

async fn system_info(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let system = ctx.client.call_value("machine.system_info", None).await?;
    let proc = ctx
        .client
        .call_value("machine.proc_stats", None)
        .await
        .unwrap_or(Value::Null);
    Ok(json_result(
        &json!({
            "system_info": system.get("system_info").cloned().unwrap_or(system),
            "proc_stats": crate::safety::redact::redact_value(&proc),
        }),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_objects_are_recognised() {
        assert!(is_temperature_object("extruder"));
        assert!(is_temperature_object("heater_bed"));
        assert!(is_temperature_object("temperature_sensor chamber"));
        assert!(is_temperature_object("temperature_fan exhaust"));
        assert!(!is_temperature_object("fan"));
        assert!(!is_temperature_object("gcode_macro HEAT"));
    }

    #[test]
    fn heater_target_detection() {
        assert!(has_target("heater_bed"));
        assert!(has_target("extruder1"));
        assert!(!has_target("temperature_sensor chamber"));
    }
}
