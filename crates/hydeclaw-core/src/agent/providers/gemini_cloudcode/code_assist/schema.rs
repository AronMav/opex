//! JSON Schema sanitization for Gemini Code Assist tool parameters.
//!
//! Gemini accepts a subset of JSON Schema. This module applies the passes
//! described in the spec (mirroring Hermes `gemini_schema.py`) to strip
//! unsupported fields before the schema reaches the API.

use serde_json::{Map, Value};

/// Gemini-allowed `format` values for string properties.
const ALLOWED_FORMATS: &[&str] = &["date-time", "enum"];

/// Sanitize a JSON Schema object for use as Gemini `functionDeclaration.parameters`.
///
/// Applies passes in order:
/// 1. Remove `additionalProperties`
/// 2. Remove `$ref` (log debug per occurrence)
/// 3. Flatten single-branch `oneOf`/`anyOf`; drop multi-branch (property removed)
/// 4. Coerce `type: "null"` / `type: ["T", "null"]`
/// 5. Strip disallowed `format` values
/// 6. Remove `examples`, `default`, `x-*` extension keys
///
/// Operates recursively so nested `properties` sub-schemas are also sanitized.
pub fn sanitize_gemini_tool_parameters(schema: Value) -> Value {
    sanitize_recursive(schema, "$")
}

fn sanitize_recursive(value: Value, path: &str) -> Value {
    match value {
        Value::Object(map) => sanitize_object(map, path),
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .enumerate()
                .map(|(i, v)| sanitize_recursive(v, &format!("{path}[{i}]")))
                .collect(),
        ),
        other => other,
    }
}

fn sanitize_object(mut map: Map<String, Value>, path: &str) -> Value {
    // ── Pass 1: Remove additionalProperties ──────────────────────────────────
    if map.remove("additionalProperties").is_some() {
        tracing::debug!(json_path = %path, "gemini-schema: removed additionalProperties");
    }

    // ── Pass 2: Remove $ref ──────────────────────────────────────────────────
    if map.remove("$ref").is_some() {
        tracing::debug!(
            json_path = %path,
            "gemini-schema: removed $ref (inline expansion not supported in MVP)"
        );
    }

    // ── Pass 6a: Remove x-* extension keys ──────────────────────────────────
    let ext_keys: Vec<String> = map
        .keys()
        .filter(|k| k.starts_with("x-"))
        .cloned()
        .collect();
    for k in ext_keys {
        map.remove(&k);
        tracing::debug!(json_path = %path, key = %k, "gemini-schema: removed extension key");
    }

    // ── Pass 6b: Remove examples and default ────────────────────────────────
    if map.remove("examples").is_some() {
        tracing::debug!(json_path = %path, "gemini-schema: removed examples");
    }
    if map.remove("default").is_some() {
        tracing::debug!(json_path = %path, "gemini-schema: removed default");
    }

    // ── Pass 4: Coerce type arrays containing null ───────────────────────────
    if let Some(type_val) = map.get("type").cloned() {
        match type_val {
            Value::String(ref s) if s == "null" => {
                tracing::debug!(json_path = %path, "gemini-schema: removed type:null");
                map.remove("type");
            }
            Value::Array(ref types) => {
                let non_null: Vec<&Value> = types
                    .iter()
                    .filter(|v| v.as_str() != Some("null"))
                    .collect();
                let has_null = types.iter().any(|v| v.as_str() == Some("null"));
                if has_null {
                    match non_null.len() {
                        0 => {
                            tracing::debug!(json_path = %path, "gemini-schema: removed type:[null]");
                            map.remove("type");
                        }
                        1 => {
                            tracing::debug!(
                                json_path = %path,
                                "gemini-schema: coerced type:[T,null] → type:T + nullable:true"
                            );
                            map.insert("type".to_string(), non_null[0].clone());
                            map.insert("nullable".to_string(), Value::Bool(true));
                        }
                        _ => {
                            // Multiple non-null types: strip null, add nullable
                            tracing::debug!(
                                json_path = %path,
                                "gemini-schema: stripped null from multi-type array"
                            );
                            let cleaned: Vec<Value> = non_null.into_iter().cloned().collect();
                            map.insert("type".to_string(), Value::Array(cleaned));
                            map.insert("nullable".to_string(), Value::Bool(true));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ── Pass 5: Strip disallowed format values ───────────────────────────────
    if let Some(fmt) = map.get("format").and_then(|v| v.as_str())
        && !ALLOWED_FORMATS.contains(&fmt)
    {
        tracing::debug!(
            json_path = %path,
            format = %fmt,
            "gemini-schema: removed disallowed format"
        );
        map.remove("format");
    }

    // ── Pass 3: Handle oneOf / anyOf in properties ───────────────────────────
    // Operates on the `properties` map: single-branch → flatten, multi → drop
    if let Some(Value::Object(props)) = map.get_mut("properties") {
        let prop_keys: Vec<String> = props.keys().cloned().collect();
        let mut to_remove: Vec<String> = Vec::new();
        let mut to_replace: Vec<(String, Value)> = Vec::new();

        for key in prop_keys {
            let prop = props.get(&key).unwrap();
            let branches_opt = prop
                .get("oneOf")
                .or_else(|| prop.get("anyOf"))
                .and_then(|v| v.as_array().cloned());

            if let Some(branches) = branches_opt {
                let prop_path = format!("{path}.properties.{key}");
                match branches.len() {
                    0 => {
                        tracing::debug!(
                            json_path = %prop_path,
                            "gemini-schema: removed property with empty oneOf/anyOf"
                        );
                        to_remove.push(key);
                    }
                    1 => {
                        tracing::debug!(
                            json_path = %prop_path,
                            "gemini-schema: flattened single-branch oneOf/anyOf"
                        );
                        to_replace.push((key, branches.into_iter().next().unwrap()));
                    }
                    _ => {
                        tracing::debug!(
                            json_path = %prop_path,
                            "gemini-schema: removed property with multi-branch oneOf/anyOf (unrepresentable)"
                        );
                        to_remove.push(key);
                    }
                }
            }
        }

        for key in to_remove {
            props.remove(&key);
        }
        for (key, replacement) in to_replace {
            props.insert(key, replacement);
        }
    }

    // ── Recurse into all remaining sub-values ────────────────────────────────
    let map: Map<String, Value> = map
        .into_iter()
        .map(|(k, v)| {
            let child_path = format!("{path}.{k}");
            (k, sanitize_recursive(v, &child_path))
        })
        .collect();

    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_additionalproperties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            },
            "additionalProperties": false
        });
        let out = sanitize_gemini_tool_parameters(schema);
        assert!(
            out.get("additionalProperties").is_none(),
            "additionalProperties must be removed, got: {out}"
        );
        assert!(out["properties"]["name"]["type"] == "string");
    }

    #[test]
    fn strips_additionalproperties_nested() {
        let schema = json!({
            "type": "object",
            "properties": {
                "inner": {
                    "type": "object",
                    "additionalProperties": true
                }
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        assert!(out["properties"]["inner"].get("additionalProperties").is_none());
    }

    #[test]
    fn flattens_oneof_with_single_branch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {
                    "oneOf": [
                        {"type": "string", "description": "A text value"}
                    ]
                }
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        let value_prop = &out["properties"]["value"];
        assert!(
            value_prop.get("oneOf").is_none(),
            "oneOf should be removed, got: {value_prop}"
        );
        assert_eq!(value_prop["type"], "string");
        assert_eq!(value_prop["description"], "A text value");
    }

    #[test]
    fn removes_oneof_with_multiple_branches() {
        let schema = json!({
            "type": "object",
            "properties": {
                "ambiguous": {
                    "oneOf": [
                        {"type": "string"},
                        {"type": "integer"}
                    ]
                },
                "kept": {"type": "boolean"}
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        let props = out["properties"].as_object().unwrap();
        assert!(
            !props.contains_key("ambiguous"),
            "ambiguous should be dropped; props: {props:?}"
        );
        assert!(props.contains_key("kept"));
    }

    #[test]
    fn expands_inline_ref_to_top_level_definition() {
        // $ref keys are removed with a debug log; no inline expansion in MVP
        let schema = json!({
            "type": "object",
            "properties": {
                "item": {"$ref": "#/definitions/Item"}
            },
            "definitions": {
                "Item": {"type": "string"}
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        assert!(
            out["properties"]["item"].get("$ref").is_none(),
            "\\$ref must be removed from property, got: {}",
            out["properties"]["item"]
        );
    }

    #[test]
    fn coerces_null_type_to_nullable() {
        let schema = json!({
            "type": "object",
            "properties": {
                "maybe_str": {"type": ["string", "null"]},
                "maybe_int": {"type": ["null", "integer"]}
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        let maybe_str = &out["properties"]["maybe_str"];
        assert_eq!(maybe_str["type"], "string");
        assert_eq!(maybe_str["nullable"], true);

        let maybe_int = &out["properties"]["maybe_int"];
        assert_eq!(maybe_int["type"], "integer");
        assert_eq!(maybe_int["nullable"], true);
    }

    #[test]
    fn removes_type_null_only_field() {
        // A property whose only type is "null" should have its type key omitted
        let schema = json!({
            "type": "object",
            "properties": {
                "void_field": {"type": "null"}
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        let void_field = &out["properties"]["void_field"];
        assert!(
            void_field.get("type").is_none(),
            "type:null must be removed, got: {void_field}"
        );
    }

    #[test]
    fn strips_disallowed_format() {
        let schema = json!({
            "type": "object",
            "properties": {
                "email": {"type": "string", "format": "email"},
                "uri": {"type": "string", "format": "uri"},
                "dt": {"type": "string", "format": "date-time"}
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        assert!(
            out["properties"]["email"].get("format").is_none(),
            "email format must be stripped"
        );
        assert!(
            out["properties"]["uri"].get("format").is_none(),
            "uri format must be stripped"
        );
        assert_eq!(
            out["properties"]["dt"]["format"],
            "date-time",
            "date-time must be preserved"
        );
    }

    #[test]
    fn removes_examples_and_default() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "examples": ["Alice", "Bob"],
                    "default": "Unknown"
                }
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        let name = &out["properties"]["name"];
        assert!(name.get("examples").is_none(), "examples must be removed");
        assert!(name.get("default").is_none(), "default must be removed");
        assert_eq!(name["type"], "string");
    }

    #[test]
    fn removes_extension_keys() {
        let schema = json!({
            "type": "object",
            "x-internal-tag": "metadata",
            "properties": {
                "val": {"type": "string", "x-validation": "strict"}
            }
        });
        let out = sanitize_gemini_tool_parameters(schema);
        assert!(
            out.get("x-internal-tag").is_none(),
            "x- key at root must be removed"
        );
        assert!(
            out["properties"]["val"].get("x-validation").is_none(),
            "x- key in property must be removed"
        );
    }

    #[test]
    fn identity_on_clean_schema() {
        // A schema with no disallowed fields must pass through unchanged
        let schema = json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"},
                "count": {"type": "integer"}
            },
            "required": ["city"]
        });
        let out = sanitize_gemini_tool_parameters(schema.clone());
        assert_eq!(out["type"], "object");
        assert_eq!(out["properties"]["city"]["type"], "string");
        assert_eq!(out["required"][0], "city");
    }
}
