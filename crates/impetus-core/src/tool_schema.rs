//! Builtin tool JSON Schemas and pre-policy argument validation.
//!
//! Schemas are the catalog source of truth for ToolOrchestrator and for
//! provider HTTP `tools` payloads (`openai_provider::openai_tools_payload`,
//! `anthropic_provider::anthropic_tools_payload`).

use std::sync::OnceLock;

use serde_json::{Map, Value};
use thiserror::Error;

/// Typed failure when model-supplied tool arguments do not match the schema.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid arguments for tool `{tool}`: {reason}")]
pub struct ToolArgError {
    pub tool: String,
    pub reason: String,
}

impl ToolArgError {
    fn new(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            reason: reason.into(),
        }
    }
}

/// Named builtin tool with a JSON Schema (draft-07 subset) for arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinToolSchema {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// All builtin tools known to [`crate::ToolOrchestrator`].
pub fn builtin_tool_schemas() -> &'static [BuiltinToolSchema] {
    schemas()
}

/// Look up a builtin schema by tool name (including aliases).
pub fn schema_for_tool(name: &str) -> Option<&'static BuiltinToolSchema> {
    let canonical = canonical_tool_name(name)?;
    schemas().iter().find(|schema| schema.name == canonical)
}

/// Map aliases (`shell`/`exec`, `edit_file`) onto the canonical schema name.
pub fn canonical_tool_name(name: &str) -> Option<&'static str> {
    match name {
        "list_files" => Some("list_files"),
        "read_file" => Some("read_file"),
        "search" => Some("search"),
        "write_file" | "edit_file" => Some("write_file"),
        "bash" | "shell" | "exec" => Some("bash"),
        "web_search" => Some("web_search"),
        "web_fetch" => Some("web_fetch"),
        "web_download" => Some("web_download"),
        "web_browser" => Some("web_browser"),
        "web_submit" => Some("web_submit"),
        "web_upload" => Some("web_upload"),
        _ => None,
    }
}

/// Validate `arguments` against the builtin schema for `tool_name`.
///
/// Returns [`ToolArgError`] for unknown tools, non-object args, missing
/// required fields, or type mismatches. Does not coerce invalid values.
pub fn validate_tool_arguments(tool_name: &str, arguments: &Value) -> Result<(), ToolArgError> {
    let schema = schema_for_tool(tool_name)
        .ok_or_else(|| ToolArgError::new(tool_name, "unknown tool (no schema registered)"))?;
    validate_against_schema(tool_name, arguments, &schema.parameters)
}

fn validate_against_schema(tool: &str, value: &Value, schema: &Value) -> Result<(), ToolArgError> {
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| ToolArgError::new(tool, "internal schema is not an object"))?;

    if let Some(expected_type) = schema_obj.get("type").and_then(Value::as_str)
        && !value_matches_type(value, expected_type)
    {
        return Err(ToolArgError::new(
            tool,
            format!(
                "expected type `{expected_type}`, got `{got}`",
                got = value_type_name(value)
            ),
        ));
    }

    if expected_type_is(schema_obj, "object") {
        let obj = value
            .as_object()
            .ok_or_else(|| ToolArgError::new(tool, "arguments must be a JSON object"))?;
        validate_object(tool, obj, schema_obj)?;
    }

    if expected_type_is(schema_obj, "string")
        && let Some(min_length) = schema_obj.get("minLength").and_then(Value::as_u64)
    {
        let len = value
            .as_str()
            .map(|s| s.chars().count() as u64)
            .unwrap_or(0);
        if len < min_length {
            return Err(ToolArgError::new(
                tool,
                format!("string shorter than minLength {min_length}"),
            ));
        }
    }

    if expected_type_is(schema_obj, "integer")
        && let Some(minimum) = schema_obj.get("minimum").and_then(Value::as_i64)
    {
        let n = value
            .as_i64()
            .ok_or_else(|| ToolArgError::new(tool, "expected integer value"))?;
        if n < minimum {
            return Err(ToolArgError::new(
                tool,
                format!("integer below minimum {minimum}"),
            ));
        }
    }

    Ok(())
}

fn validate_object(
    tool: &str,
    obj: &Map<String, Value>,
    schema_obj: &Map<String, Value>,
) -> Result<(), ToolArgError> {
    if let Some(required) = schema_obj.get("required").and_then(Value::as_array) {
        for key in required {
            let Some(name) = key.as_str() else {
                continue;
            };
            if !obj.contains_key(name) {
                return Err(ToolArgError::new(
                    tool,
                    format!("missing required property `{name}`"),
                ));
            }
        }
    }

    let properties = schema_obj
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let additional = schema_obj
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    for (key, value) in obj {
        match properties.get(key) {
            Some(prop_schema) => {
                validate_against_schema(tool, value, prop_schema).map_err(|err| {
                    ToolArgError::new(tool, format!("property `{key}`: {}", err.reason))
                })?;
            }
            None if !additional => {
                return Err(ToolArgError::new(
                    tool,
                    format!("unexpected property `{key}`"),
                ));
            }
            None => {}
        }
    }

    Ok(())
}

fn expected_type_is(schema_obj: &Map<String, Value>, expected: &str) -> bool {
    schema_obj
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t == expected)
}

fn value_matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn object_schema(required: &[&str], properties: Value) -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    })
}

fn string_prop(description: &str) -> Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "description": description,
    })
}

fn schemas() -> &'static [BuiltinToolSchema] {
    static SCHEMAS: OnceLock<Vec<BuiltinToolSchema>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            vec![
                BuiltinToolSchema {
                    name: "list_files",
                    description: "List workspace directory entries",
                    parameters: object_schema(
                        &[],
                        serde_json::json!({
                            "path": string_prop("Directory path relative to workspace (default: .)"),
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "read_file",
                    description: "Read a workspace file",
                    parameters: object_schema(
                        &["path"],
                        serde_json::json!({
                            "path": string_prop("File path relative to workspace"),
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "search",
                    description: "Search workspace content",
                    parameters: object_schema(
                        &["pattern"],
                        serde_json::json!({
                            "pattern": string_prop("Search pattern"),
                            "path": string_prop("Optional root path relative to workspace"),
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "write_file",
                    description: "Create or overwrite a workspace file (policy-gated)",
                    parameters: object_schema(
                        &["path", "content"],
                        serde_json::json!({
                            "path": string_prop("File path relative to workspace"),
                            "content": {
                                "type": "string",
                                "description": "File contents to write"
                            },
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "bash",
                    description: "Run a shell command (policy-gated)",
                    parameters: object_schema(
                        &["command"],
                        serde_json::json!({
                            "command": string_prop("Shell command to execute"),
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "web_search",
                    description: "Search the public web",
                    parameters: object_schema(
                        &["query"],
                        serde_json::json!({
                            "query": string_prop("Search query"),
                            "max_results": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "Maximum number of results"
                            },
                            "locale": { "type": "string", "description": "Optional locale hint" },
                            "safe_search": {
                                "type": "string",
                                "description": "Safe search preference"
                            },
                            "backend": {
                                "description": "Search backend preference (string or tagged object)"
                            },
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "web_fetch",
                    description: "Fetch a public URL",
                    parameters: object_schema(
                        &["url"],
                        serde_json::json!({
                            "url": string_prop("Absolute http(s) URL"),
                            "max_bytes": { "type": "integer", "minimum": 1 },
                            "max_chars": { "type": "integer", "minimum": 1 },
                            "include_links": { "type": "boolean" },
                            "allow_binary_metadata": { "type": "boolean" },
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "web_download",
                    description: "Download a remote file (policy-gated; executor may be unavailable)",
                    parameters: object_schema(
                        &["url"],
                        serde_json::json!({
                            "url": string_prop("Absolute http(s) URL"),
                            "path": string_prop("Optional destination path"),
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "web_browser",
                    description: "Open a browser session (policy-gated; executor may be unavailable)",
                    parameters: object_schema(
                        &["url"],
                        serde_json::json!({
                            "url": string_prop("Absolute http(s) URL"),
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "web_submit",
                    description: "Submit a web form (policy-gated; executor may be unavailable)",
                    parameters: object_schema(
                        &["url"],
                        serde_json::json!({
                            "url": string_prop("Absolute http(s) URL"),
                            "body": { "description": "Optional form payload" },
                        }),
                    ),
                },
                BuiltinToolSchema {
                    name: "web_upload",
                    description: "Upload a file over the web (policy-gated; executor may be unavailable)",
                    parameters: object_schema(
                        &["url", "path"],
                        serde_json::json!({
                            "url": string_prop("Absolute http(s) URL"),
                            "path": string_prop("Local file path to upload"),
                        }),
                    ),
                },
            ]
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_read_file_passes() {
        validate_tool_arguments("read_file", &serde_json::json!({"path": "a.txt"})).expect("valid");
    }

    #[test]
    fn missing_required_rejected() {
        let err =
            validate_tool_arguments("read_file", &serde_json::json!({})).expect_err("missing path");
        assert!(err.reason.contains("missing required property `path`"));
        assert_eq!(err.tool, "read_file");
    }

    #[test]
    fn empty_string_required_rejected() {
        let err = validate_tool_arguments("bash", &serde_json::json!({"command": ""}))
            .expect_err("empty command");
        assert!(err.reason.contains("minLength"));
    }

    #[test]
    fn wrong_type_rejected_without_coercion() {
        let err = validate_tool_arguments("read_file", &serde_json::json!({"path": 42}))
            .expect_err("path must be string");
        assert!(err.reason.contains("expected type `string`"));
    }

    #[test]
    fn non_object_arguments_rejected() {
        let err = validate_tool_arguments("list_files", &serde_json::json!("nope"))
            .expect_err("must be object");
        assert!(err.reason.contains("expected type `object`"));
    }

    #[test]
    fn aliases_share_canonical_schema() {
        validate_tool_arguments("shell", &serde_json::json!({"command": "ls"}))
            .expect("shell alias");
        validate_tool_arguments(
            "edit_file",
            &serde_json::json!({"path": "a.txt", "content": "x"}),
        )
        .expect("edit_file alias");
    }

    #[test]
    fn unknown_tool_has_no_schema() {
        let err =
            validate_tool_arguments("not_a_tool", &serde_json::json!({})).expect_err("unknown");
        assert!(err.reason.contains("unknown tool"));
    }

    #[test]
    fn error_does_not_echo_secret_argument_values() {
        let err = validate_tool_arguments(
            "write_file",
            &serde_json::json!({"path": "x", "content": 123}),
        )
        .expect_err("bad content type");
        let rendered = err.to_string();
        assert!(!rendered.contains("sk-secret"));
        assert!(!rendered.contains("password"));
        assert!(rendered.contains("property `content`"));
    }

    #[test]
    fn catalog_covers_orchestrator_tools() {
        for name in [
            "list_files",
            "read_file",
            "search",
            "write_file",
            "bash",
            "web_search",
            "web_fetch",
            "web_download",
            "web_browser",
            "web_submit",
            "web_upload",
        ] {
            assert!(schema_for_tool(name).is_some(), "missing schema for {name}");
        }
        assert_eq!(builtin_tool_schemas().len(), 11);
    }
}
