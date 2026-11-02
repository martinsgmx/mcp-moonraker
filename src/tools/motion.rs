//! Motion, speed/flow factors, and macro execution.
//!
//! `moonraker_move_axis` deliberately supports X/Y jogs and upward Z moves only.
//! Downward Z movement is a common way to crash a nozzle into a bed or print, so
//! it is not exposed as a casual tool. Use a dedicated macro (which requires
//! approval) if you need controlled downward motion.

use serde_json::{json, Value};

use crate::{
    error::{Error, Result},
    safety::{Capability, Risk},
    tools::{
        args::{gcode_quote, validate_object_name, Args},
        empty_schema, json_result, no_summary, object_names, query_objects, require_object,
        schema_object, Gate, ToolContext, ToolSpec,
    },
};

/// Motion tool specifications.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "moonraker_list_macros",
            title: "List loaded Klipper macros",
            description: "List the `gcode_macro` objects currently loaded in Klipper, with their \
                          defined variables when present. Use this before moonraker_run_macro to \
                          pick a valid macro name. Read-only.",
            allowed_args: &[],
            capability: Capability::Read,
            risk: Risk::Read,
            gate: Gate::Policy,
            summarize: no_summary,
            schema: empty_schema,
            handler: |ctx, _args| Box::pin(list_macros(ctx)),
        },
        ToolSpec {
            name: "moonraker_home_axes",
            title: "Home printer axes",
            description: "Run G28 for the selected axes. Defaults to all axes. Homing moves the \
                          toolhead, so ensure the bed is clear. This is a reversible mutation and \
                          does not require approval.",
            allowed_args: &["axes"],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: summarize_home,
            schema: home_schema,
            handler: |ctx, args| Box::pin(home_axes(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_move_axis",
            title: "Jog an axis",
            description:
                "Move X or Y by a relative amount, or move Z upward. The axis must already \
                          be homed. Downward Z moves are intentionally rejected. The G-code \
                          coordinate state is saved and restored around the move. Does not require \
                          approval.",
            allowed_args: &["axis", "distance", "speed"],
            capability: Capability::Mutation,
            risk: Risk::Medium,
            gate: Gate::Policy,
            summarize: summarize_move,
            schema: move_schema,
            handler: |ctx, args| Box::pin(move_axis(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_set_speed_factor",
            title: "Set print speed factor",
            description: "Set Klipper's speed factor with M220 (percentage, 10-200). This changes \
                          feedrate scaling for subsequent moves and is reversible.",
            allowed_args: &["percent"],
            capability: Capability::Mutation,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: summarize_speed,
            schema: || factor_schema("percent", 10, 200, "Speed factor percentage."),
            handler: |ctx, args| Box::pin(set_speed_factor(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_set_flow_factor",
            title: "Set extrusion flow factor",
            description:
                "Set Klipper's extrusion flow factor with M221 (percentage, 50-150). This \
                          changes extrusion scaling for subsequent moves and is reversible.",
            allowed_args: &["percent"],
            capability: Capability::Mutation,
            risk: Risk::Low,
            gate: Gate::Policy,
            summarize: summarize_flow,
            schema: || factor_schema("percent", 50, 150, "Flow factor percentage."),
            handler: |ctx, args| Box::pin(set_flow_factor(ctx, args)),
        },
        ToolSpec {
            name: "moonraker_run_macro",
            title: "Run a Klipper macro",
            description: "DESTRUCTIVE: execute a loaded `gcode_macro` by name, with optional \
                           parameters (for example BED=110). Macros can perform arbitrary actions \
                           (including heating and motion), so this always requires double \
                           confirmation. The first call returns an approval prompt; call \
                           moonraker_confirm_action to execute.",
            allowed_args: &["name", "params"],
            capability: Capability::Mutation,
            risk: Risk::High,
            gate: Gate::Policy,
            summarize: summarize_macro,
            schema: run_macro_schema,
            handler: |ctx, args| Box::pin(run_macro(ctx, args)),
        },
    ]
}

fn home_schema() -> Value {
    schema_object(
        json!({
            "axes": {
                "type": "string",
                "default": "xyz",
                "description": "Axes to home: any combination of x, y, and z (for example 'xy' or 'z')."
            }
        }),
        &[],
    )
}

fn summarize_home(args: &Value) -> String {
    let axes = args.get("axes").and_then(Value::as_str).unwrap_or("xyz");
    format!("Home printer axes: {axes}.")
}

async fn home_axes(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["axes"])?;
    let axes = args
        .optional_string("axes")?
        .unwrap_or_else(|| "xyz".into())
        .to_ascii_lowercase();
    if axes.is_empty()
        || axes.len() > 3
        || !axes.chars().all(|ch| matches!(ch, 'x' | 'y' | 'z'))
        || axes.chars().any(|ch| axes.matches(ch).count() > 1)
    {
        return Err(Error::InvalidArguments(
            "axes must be a unique combination of x, y, and z".to_string(),
        ));
    }

    let script = format!(
        "G28 {}",
        axes.to_ascii_uppercase()
            .chars()
            .map(|ch| ch.to_string())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let response = ctx.client.call_gcode_script(&script).await?;
    Ok(json_result(
        &json!({"axes": axes, "result": response}),
        None,
    ))
}

fn move_schema() -> Value {
    schema_object(
        json!({
            "axis": {
                "type": "string",
                "enum": ["x", "y", "z"],
                "description": "Axis to jog. Z is upward-only."
            },
            "distance": {
                "type": "number",
                "minimum": -100,
                "maximum": 100,
                "description": "Relative distance in millimetres. Z must be positive."
            },
            "speed": {
                "type": "number",
                "minimum": 1,
                "maximum": 600,
                "default": 60,
                "description": "Speed in mm/s."
            }
        }),
        &["axis", "distance"],
    )
}

fn summarize_move(args: &Value) -> String {
    let axis = args.get("axis").and_then(Value::as_str).unwrap_or("?");
    let distance = args
        .get("distance")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let speed = args.get("speed").and_then(Value::as_f64).unwrap_or(60.0);
    format!("Move {axis} by {distance:.2} mm at {speed:.0} mm/s.")
}

async fn move_axis(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["axis", "distance", "speed"])?;
    let axis = args.required_string("axis")?.to_ascii_lowercase();
    let distance = args.required_f64("distance")?;
    let speed = args.optional_f64("speed")?.unwrap_or(60.0);

    let axis = match axis.as_str() {
        "x" | "y" | "z" => axis,
        _ => {
            return Err(Error::InvalidArguments(
                "axis must be 'x', 'y', or 'z'".to_string(),
            ));
        }
    };
    if !(1.0..=600.0).contains(&speed) {
        return Err(Error::InvalidArguments(
            "speed must be between 1 and 600 mm/s".to_string(),
        ));
    }
    if !(-100.0..=100.0).contains(&distance) || distance == 0.0 {
        return Err(Error::InvalidArguments(
            "distance must be a non-zero value between -100 and 100 mm".to_string(),
        ));
    }
    if axis == "z" && distance < 0.0 {
        return Err(Error::InvalidArguments(
            "downward Z movement is not supported by this tool; use a vetted macro if you \
             intentionally need to lower Z"
                .to_string(),
        ));
    }

    let homed = query_objects(
        ctx,
        [("toolhead".to_string(), json!(["homed_axes"]))]
            .into_iter()
            .collect(),
    )
    .await?;
    let homed_axes = homed
        .get("status")
        .and_then(|status| status.get("toolhead"))
        .and_then(|toolhead| toolhead.get("homed_axes"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !homed_axes.contains(&axis) {
        return Err(Error::InvalidArguments(format!(
            "axis '{axis}' is not homed; run moonraker_home_axes first (homed axes: \
             '{}')",
            if homed_axes.is_empty() {
                "none"
            } else {
                &homed_axes
            }
        )));
    }

    let feed = speed * 60.0;
    let script = format!(
        "SAVE_GCODE_STATE NAME=mcp_moonraker_move\nG91\nG1 {axis}{distance:.3} F{feed:.0}\n\
         RESTORE_GCODE_STATE NAME=mcp_moonraker_move MOVE=0"
    );
    let response = ctx.client.call_gcode_script(&script).await?;

    Ok(json_result(
        &json!({
            "axis": axis,
            "distance_mm": distance,
            "speed_mm_s": speed,
            "result": response,
        }),
        None,
    ))
}

fn factor_schema(key: &str, min: u64, max: u64, description: &str) -> Value {
    schema_object(
        json!({
            key: {
                "type": "number",
                "minimum": min,
                "maximum": max,
                "description": description
            }
        }),
        &[key],
    )
}

fn summarize_speed(args: &Value) -> String {
    format!(
        "Set speed factor to {}%.",
        args.get("percent")
            .and_then(Value::as_f64)
            .unwrap_or_default()
    )
}

fn summarize_flow(args: &Value) -> String {
    format!(
        "Set flow factor to {}%.",
        args.get("percent")
            .and_then(Value::as_f64)
            .unwrap_or_default()
    )
}

async fn set_speed_factor(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["percent"])?;
    let percent = args.required_f64("percent")?;
    if !(10.0..=200.0).contains(&percent) {
        return Err(Error::InvalidArguments(
            "percent must be between 10 and 200".to_string(),
        ));
    }
    let response = ctx
        .client
        .call_gcode_script(&format!("M220 S{percent:.1}"))
        .await?;
    Ok(json_result(
        &json!({"percent": percent, "result": response}),
        None,
    ))
}

async fn set_flow_factor(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let args = Args::parse(Some(arguments), &["percent"])?;
    let percent = args.required_f64("percent")?;
    if !(50.0..=150.0).contains(&percent) {
        return Err(Error::InvalidArguments(
            "percent must be between 50 and 150".to_string(),
        ));
    }
    let response = ctx
        .client
        .call_gcode_script(&format!("M221 S{percent:.1}"))
        .await?;
    Ok(json_result(
        &json!({"percent": percent, "result": response}),
        None,
    ))
}

fn macro_schema() -> Value {
    schema_object(
        json!({
            "name": {
                "type": "string",
                "description": "Macro name as shown after 'gcode_macro ' in the object list, for example 'PRINT_START'."
            }
        }),
        &["name"],
    )
}

fn run_macro_schema() -> Value {
    let mut schema = macro_schema();
    if let Some(map) = schema.as_object_mut() {
        map.insert(
            "params".to_string(),
            json!({
                "type": "object",
                "additionalProperties": {
                    "type": ["string", "number", "boolean"]
                },
                "maxProperties": 16,
                "description": "Optional macro parameters as key/value pairs, rendered as NAME KEY=VALUE. Numbers are passed unquoted, strings are G-code quoted, booleans become 1/0."
            }),
        );
    }
    schema
}

fn summarize_macro(args: &Value) -> String {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let params = args.get("params").and_then(Value::as_object);
    match params {
        Some(map) if !map.is_empty() => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            format!(
                "Execute Klipper macro '{name}' with parameters: {}.",
                keys.join(", ")
            )
        }
        _ => format!("Execute Klipper macro '{name}'."),
    }
}

async fn run_macro(
    ctx: &ToolContext,
    arguments: Value,
) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let params = macro_params(&arguments)?;
    let args = Args::parse(Some(arguments), &["name", "params"])?;
    let name = args.required_string("name")?.to_ascii_uppercase();
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(Error::InvalidArguments(
            "macro name must contain only letters, digits, '_' and '-'".to_string(),
        ));
    }

    let object = format!("gcode_macro {name}");
    let names = object_names(ctx).await?;
    if !names.iter().any(|candidate| candidate == &object) {
        let available = names
            .iter()
            .filter_map(|candidate| candidate.strip_prefix("gcode_macro "))
            .take(40)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::InvalidArguments(format!(
            "macro '{name}' is not loaded. Available macros include: {available}"
        )));
    }

    let script = if params.is_empty() {
        name.clone()
    } else {
        format!("{} {}", name, params.join(" "))
    };
    let response = ctx.client.call_gcode_script(&script).await?;
    Ok(json_result(
        &json!({
            "macro": name,
            "script": script,
            "result": response,
        }),
        None,
    ))
}

/// Maximum accepted macro parameters; silently tighter than the schema cap.
const MAX_MACRO_PARAMS: usize = 16;

/// Extract and validate optional macro `params` from the raw arguments.
///
/// Keys become G-code parameter names, so they must be plain identifiers.
/// Values are limited to strings, numbers, and booleans; strings are G-code
/// quoted, booleans become `1`/`0`.
fn macro_params(arguments: &Value) -> Result<Vec<String>> {
    let Some(map) = arguments
        .get("params")
        .filter(|value| !value.is_null())
        .and_then(Value::as_object)
    else {
        return Ok(Vec::new());
    };
    if map.len() > MAX_MACRO_PARAMS {
        return Err(Error::InvalidArguments(format!(
            "at most {MAX_MACRO_PARAMS} macro parameters are allowed"
        )));
    }
    let mut rendered = Vec::with_capacity(map.len());
    for (key, value) in map {
        if key.is_empty()
            || key.len() > 32
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return Err(Error::InvalidArguments(
                "macro parameter names must contain only letters, digits and '_'".to_string(),
            ));
        }
        let text = match value {
            Value::String(text) => gcode_quote(text),
            Value::Bool(true) => "1".to_string(),
            Value::Bool(false) => "0".to_string(),
            Value::Number(number) if number.is_f64() || number.is_i64() || number.is_u64() => {
                number.to_string()
            }
            _ => {
                return Err(Error::InvalidArguments(
                    "macro parameter values must be strings, numbers, or booleans".to_string(),
                ));
            }
        };
        rendered.push(format!("{key}={text}"));
    }
    Ok(rendered)
}

async fn list_macros(ctx: &ToolContext) -> Result<rust_mcp_sdk::schema::CallToolResult> {
    let names = object_names(ctx).await?;
    let macros: Vec<String> = names
        .iter()
        .filter_map(|name| name.strip_prefix("gcode_macro ").map(str::to_string))
        .collect();

    let values: Vec<Value> = if macros.is_empty() {
        Vec::new()
    } else {
        let objects = macros
            .iter()
            .map(|name| (format!("gcode_macro {name}"), json!(null)))
            .collect::<serde_json::Map<String, Value>>();
        // Variables (if defined) come back in the status payload; a failed or
        // empty query degrades to names-only output.
        query_objects(ctx, objects)
            .await
            .ok()
            .and_then(|status| status.get("status").cloned())
            .and_then(|status| status.as_object().cloned())
            .map_or_else(
                || macros.iter().map(|name| json!({"name": name})).collect(),
                |status| {
                    macros
                        .iter()
                        .map(|name| {
                            let object = format!("gcode_macro {name}");
                            json!({
                                "name": name,
                                "variables": status
                                    .get(&object)
                                    .and_then(|entry| entry.get("variables"))
                                    .cloned()
                                    .unwrap_or(Value::Null),
                            })
                        })
                        .collect()
                },
            )
    };

    Ok(json_result(
        &json!({"count": values.len(), "macros": values}),
        None,
    ))
}

/// Validate that a named object exists; used by tests and future tools.
pub async fn ensure_object(ctx: &ToolContext, name: &str) -> Result<()> {
    let validated = validate_object_name(name, "object")?;
    require_object(ctx, &validated, "object").await
}

/// Quote helper kept public for macro tools that add parameters later.
#[must_use]
pub fn quoted(value: &str) -> String {
    gcode_quote(value)
}
