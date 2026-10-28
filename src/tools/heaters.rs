//! Heater and fan controls.
//!
//! Heater targets are validated against loaded Klipper objects *and* clamped by
//! the local policy limit. The object name is always resolved from Klipper's
//! object list before it is interpolated into a G-code command, so a hostile
//! argument cannot inject commands.

use serde_json::{json, Value};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{
        args::{gcode_quote, validate_object_name, Args},
        json_result, object_names, query_objects, require_object, schema_object, Gate, ToolContext,
        ToolSpec,
    },
};

/// Heater and fan tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_set_heater",
            title: "Set a heater target",
            description: "Set the target temperature of a heater or the heated bed. The heater \
                          name must be a loaded Klipper object (for example 'extruder' or \
                          'heater_bed'). Targets are capped by the server's configured safety \
                          limits. Use temperature 0 to turn a heater off. This is a reversible \
                          mutation and does not require approval.",
            allowed_args: &["heater", "temperature"],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: summarize_set_heater,
            schema: set_heater_schema,
            handler: |ctx, args| Box::pin(set_heater(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_turn_off_heaters",
            title: "Turn off all heaters",
            description: "Turn off every heater and the heated bed immediately. This is a \
                          reversible safety-oriented mutation and does not require approval.",
            allowed_args: &[],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: |_| "Turn off all heaters.".to_string(),
            schema: crate::tools::empty_schema,
            handler: |ctx, _args| Box::pin(turn_off_heaters(ctx)),
        },
        ToolSpec {
            name: "moonraker_set_fan",
            title: "Set a fan speed",
            description: "Set a controllable fan speed by percent (0-100). Supported fans are the \
                          part-cooling fan ('fan', controlled with M106) and fan_generic objects \
                          (controlled with SET_FAN_SPEED). Automatic fans such as heater_fan and \
                          temperature_fan are rejected because Klipper manages them.",
            allowed_args: &["fan", "percent"],
            capability: Capability::Mutation,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: summarize_set_fan,
            schema: set_fan_schema,
            handler: |ctx, args| Box::pin(set_fan(ctx, args)),
        },
    ]
}

fn set_heater_schema() -> Value {
    schema_object(
        json!({
            "heater": {
                "type": "string",
                "description": "Loaded Klipper heater object, for example 'extruder', 'extruder1', or 'heater_bed'."
            },
            "temperature": {
                "type": "number",
                "minimum": 0,
                "maximum": 500,
                "description": "Target temperature in degrees Celsius. Use 0 to turn the heater off."
            }
        }),
        &["heater", "temperature"],
    )
}

fn summarize_set_heater(args: &Value) -> String {
    let heater = args
        .get("heater")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let temperature = args
        .get("temperature")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    format!("Set heater '{heater}' target to {temperature:.1} °C.")
}

async fn set_heater(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["heater", "temperature"])?;
    let heater = validate_object_name(&args.required_string("heater")?, "heater")?;
    let temperature = args.required_f64("temperature")?;

    if !is_heater(&heater) {
        return Err(Error::InvalidArguments(format!(
            "'{heater}' is not a heater object; use moonraker_temperatures to list heaters"
        )));
    }
    require_object(ctx, &heater, "heater").await?;

    let limit = ctx.policy.max_temp_for(&heater);
    if temperature < 0.0 || temperature > limit {
        return Err(Error::InvalidArguments(format!(
            "target {temperature:.1} °C is outside the allowed range 0-{limit:.1} °C for '{heater}'"
        )));
    }

    let script = format!(
        "SET_HEATER_TEMPERATURE HEATER={} TARGET={:.1}",
        gcode_quote(&heater),
        temperature
    );
    let response = ctx
        .client
        .call_value("printer.gcode.script", Some(json!({"script": script})))
        .await?;

    let mut objects = std::collections::BTreeMap::new();
    objects.insert(heater.clone(), json!(["temperature", "target", "power"]));
    let state = query_objects(ctx, objects.into_iter().collect())
        .await
        .ok()
        .and_then(|value| value.get("status").cloned());

    if temperature == 0.0 {
        Ok(json_result(
            &json!({
                "heater": heater,
                "requested_target": 0.0,
                "state": state,
                "result": response,
            }),
            Some("Heater turned off."),
        ))
    } else {
        Ok(json_result(
            &json!({
                "heater": heater,
                "requested_target": temperature,
                "state": state,
                "result": response,
            }),
            Some(&format!(
                "Target for '{heater}' set to {temperature:.1} °C. The printer will report actual \
                 temperature separately; use moonraker_temperatures to verify."
            )),
        ))
    }
}

async fn turn_off_heaters(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let response = ctx
        .client
        .call_value(
            "printer.gcode.script",
            Some(json!({"script": "TURN_OFF_HEATERS"})),
        )
        .await?;
    Ok(json_result(
        &json!({"result": response}),
        Some("All heaters have been turned off."),
    ))
}

fn is_heater(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "extruder"
        || lower.starts_with("extruder")
        || lower == "heater_bed"
        || lower.starts_with("heater_generic")
        || lower.starts_with("heater_")
}

fn set_fan_schema() -> Value {
    schema_object(
        json!({
            "fan": {
                "type": "string",
                "default": "fan",
                "description": "Fan object name. Use 'fan' for part cooling, or a fan_generic object."
            },
            "percent": {
                "type": "number",
                "minimum": 0,
                "maximum": 100,
                "description": "Fan speed as a percentage."
            }
        }),
        &["percent"],
    )
}

fn summarize_set_fan(args: &Value) -> String {
    let fan = args.get("fan").and_then(Value::as_str).unwrap_or("fan");
    let percent = args
        .get("percent")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    format!("Set fan '{fan}' to {percent:.0}%.")
}

async fn set_fan(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["fan", "percent"])?;
    let fan = validate_object_name(
        &args.optional_string("fan")?.unwrap_or_else(|| "fan".into()),
        "fan",
    )?;
    let percent = args.required_f64("percent")?;
    if !(0.0..=100.0).contains(&percent) {
        return Err(Error::InvalidArguments(
            "percent must be between 0 and 100".to_string(),
        ));
    }

    let names = object_names(ctx).await?;
    if !names.iter().any(|name| name == &fan) {
        return Err(Error::InvalidArguments(format!(
            "fan '{fan}' is not a loaded Klipper object"
        )));
    }

    let script = if fan == "fan" {
        // `percent` is validated to 0..=100 above, so the clamp guarantees the
        // value is exactly representable as 0..=255 before the cast.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pwm = (percent / 100.0 * 255.0).round().clamp(0.0, 255.0) as u16;
        format!("M106 S{pwm}")
    } else if fan.starts_with("fan_generic") {
        format!(
            "SET_FAN_SPEED FAN={} SPEED={:.4}",
            gcode_quote(&fan),
            percent / 100.0
        )
    } else {
        return Err(Error::InvalidArguments(format!(
            "'{fan}' is not directly controllable through this tool; only 'fan' and \
             'fan_generic ...' objects are supported"
        )));
    };

    let response = ctx
        .client
        .call_value("printer.gcode.script", Some(json!({"script": script})))
        .await?;
    Ok(json_result(
        &json!({
            "fan": fan,
            "percent": percent,
            "result": response,
        }),
        None,
    ))
}
