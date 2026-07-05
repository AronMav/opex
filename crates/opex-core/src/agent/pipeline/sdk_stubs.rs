//! SDK stub generation for codemode (tools-as-code).
//!
//! Generates a Python preamble file (`opex_sdk.py`) from the agent's filtered
//! tool list. The preamble is injected into the code_exec sandbox so a script
//! can call `tools.workspace_read(path="foo.txt")` and it dispatches through
//! the loopback `/api/sandbox/tool-call` endpoint.
//!
//! Stubs are generated from [`opex_types::ToolDefinition`] — `input_schema`
//! (JSON Schema) is mapped to Python parameter names + types + docstrings.

use opex_types::ToolDefinition;

/// Generate the Python SDK preamble from a filtered tool list.
///
/// The preamble defines a `tools` object with one method per tool. Each method
/// calls `_call(tool_name, arguments)` which POSTs to the loopback endpoint
/// with the `X-Codemode-Token` from the environment.
pub fn generate_python_sdk(tools: &[ToolDefinition]) -> String {
    let mut out = String::new();
    out.push_str("# Auto-generated OPEX tool SDK. Do not edit.\n");
    out.push_str("# Call tools via: tools.tool_name(arg1=\"...\", arg2=\"...\")\n");
    out.push_str("import os, json, urllib.request, urllib.error\n\n");
    out.push_str("_TOKEN = os.environ.get(\"OPEX_CODEMODE_TOKEN\", \"\")\n");
    out.push_str("_URL = os.environ.get(\"OPEX_CODEMODE_URL\", \"http://localhost:18789/api/sandbox/tool-call\")\n");
    out.push_str("_SESSION = os.environ.get(\"OPEX_SESSION_ID\", \"\")\n");
    out.push_str("_AGENT = os.environ.get(\"OPEX_AGENT_NAME\", \"\")\n");
    out.push_str("_call_index = [0]\n\n");
    out.push_str("def _call(tool, arguments):\n");
    out.push_str("    idx = _call_index[0]; _call_index[0] += 1\n");
    out.push_str("    body = json.dumps({\n");
    out.push_str("        \"tool\": tool,\n");
    out.push_str("        \"arguments\": arguments,\n");
    out.push_str("        \"_ctx\": {\"session_id\": _SESSION, \"agent_name\": _AGENT, \"call_index\": idx},\n");
    out.push_str("    }).encode()\n");
    out.push_str("    req = urllib.request.Request(_URL, data=body,\n");
    out.push_str("        headers={\"Content-Type\": \"application/json\", \"X-Codemode-Token\": _TOKEN})\n");
    out.push_str("    try:\n");
    out.push_str("        with urllib.request.urlopen(req, timeout=120) as r:\n");
    out.push_str("            resp = json.loads(r.read())\n");
    out.push_str("            return resp.get(\"result\", \"\")\n");
    out.push_str("    except urllib.error.HTTPError as e:\n");
    out.push_str("        body = e.read().decode()\n");
    out.push_str("        try:\n");
    out.push_str("            err = json.loads(body)\n");
    out.push_str("            raise RuntimeError(err.get(\"error\", body)) from None\n");
    out.push_str("        except json.JSONDecodeError:\n");
    out.push_str("            raise RuntimeError(f\"HTTP {e.code}: {body}\") from None\n\n");
    out.push_str("def tools_search(query, limit=10):\n");
    out.push_str("    \"\"\"Search available tools by keyword. Returns matching tool signatures.\"\"\"\n");
    out.push_str("    idx = _call_index[0]; _call_index[0] += 1\n");
    out.push_str("    search_url = _URL.replace(\"/tool-call\", \"/tool-search\")\n");
    out.push_str("    body = json.dumps({\n");
    out.push_str("        \"query\": query, \"limit\": limit,\n");
    out.push_str("        \"_ctx\": {\"session_id\": _SESSION, \"agent_name\": _AGENT, \"call_index\": idx},\n");
    out.push_str("    }).encode()\n");
    out.push_str("    req = urllib.request.Request(search_url, data=body,\n");
    out.push_str("        headers={\"Content-Type\": \"application/json\", \"X-Codemode-Token\": _TOKEN})\n");
    out.push_str("    try:\n");
    out.push_str("        with urllib.request.urlopen(req, timeout=30) as r:\n");
    out.push_str("            return json.loads(r.read())\n");
    out.push_str("    except urllib.error.HTTPError as e:\n");
    out.push_str("        body = e.read().decode()\n");
    out.push_str("        try:\n");
    out.push_str("            err = json.loads(body)\n");
    out.push_str("            raise RuntimeError(err.get(\"error\", body)) from None\n");
    out.push_str("        except json.JSONDecodeError:\n");
    out.push_str("            raise RuntimeError(f\"HTTP {e.code}: {body}\") from None\n\n");
    out.push_str("class _Tools:\n");

    for tool in tools {
        // Skip tool names that would produce an invalid Python identifier
        // (M3: e.g. `foo.bar`, `1tool`, `foo bar`), which would break the
        // entire preamble with a SyntaxError. The script can still call these
        // tools via `_call("tool.name", {...})` directly.
        if !is_valid_method_name(&tool.name) {
            continue;
        }
        let py_sig = generate_python_signature(tool);
        let doc = escape_docstring(&tool.description);
        out.push_str(&format!("    def {py_sig}:\n"));
        out.push_str(&format!("        \"\"\"{doc}\"\"\"\n"));
        out.push_str(&format!(
            "        return _call(\"{}\", {{{}}})\n\n",
            tool.name,
            generate_python_call_args(tool)
        ));
    }

    out.push_str("tools = _Tools()\n");
    out
}

/// Generate a Python method signature from a ToolDefinition's input_schema.
/// e.g. `workspace_read(self, path: str) -> str`
fn generate_python_signature(tool: &ToolDefinition) -> String {
    let params = python_params(tool);
    let mut sig = format!("def {}", sanitize_method_name(&tool.name));
    sig.push('(');
    sig.push_str("self");
    for (name, type_hint, required) in &params {
        if *required {
            sig.push_str(&format!(", {name}: {type_hint}"));
        } else {
            sig.push_str(&format!(", {name}: {type_hint} = None"));
        }
    }
    sig.push_str(") -> str");
    sig
}

/// Generate the Python dict-literal of arguments for the _call body.
/// e.g. `"path": path, "content": content`
fn generate_python_call_args(tool: &ToolDefinition) -> String {
    let params = python_params(tool);
    params
        .iter()
        .map(|(name, _, _)| format!("\"{name}\": {name}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Extract Python (name, type_hint, required) tuples from input_schema.
fn python_params(tool: &ToolDefinition) -> Vec<(String, String, bool)> {
    let Some(props) = tool.input_schema.get("properties").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let required: Vec<&str> = tool
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for (name, schema) in props {
        let py_type = json_type_to_python(schema);
        let is_required = required.contains(&name.as_str());
        out.push((name.clone(), py_type, is_required));
    }
    out
}

/// Map a JSON Schema type to a Python type hint.
fn json_type_to_python(schema: &serde_json::Value) -> String {
    match schema.get("type").and_then(|t| t.as_str()) {
        Some("string") => "str".into(),
        Some("integer") => "int".into(),
        Some("number") => "float".into(),
        Some("boolean") => "bool".into(),
        Some("array") => "list".into(),
        Some("object") => "dict".into(),
        _ => "Any".into(),
    }
}

/// Sanitize a tool name into a valid Python method name.
/// Tool names are `[a-zA-Z0-9_-]` — replace `-` with `_`, then validate the
/// result is a legal Python identifier (`^[A-Za-z_][A-Za-z0-9_]*$`). Names
/// that don't satisfy this (e.g. a YAML tool named `foo.bar` or `1tool`)
/// would produce a `SyntaxError` in the generated preamble, breaking the
/// entire SDK. Such names are skipped by the caller via `is_valid_method_name`.
fn sanitize_method_name(name: &str) -> String {
    name.replace('-', "_")
}

/// Check if a tool name produces a valid Python identifier after sanitization.
/// Names like `foo.bar`, `1tool`, `foo bar` are rejected so they don't break
/// the entire generated preamble with a `SyntaxError`.
fn is_valid_method_name(name: &str) -> bool {
    let sanitized = sanitize_method_name(name);
    let mut chars = sanitized.chars();
    let Some(first) = chars.next() else { return false };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Escape a description for use in a Python docstring (no triple-quotes).
fn escape_docstring(desc: &str) -> String {
    desc.replace("\\", "\\\\").replace("\"\"\"", "\\\"\\\"\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, desc: &str, schema: serde_json::Value) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: desc.into(),
            input_schema: schema,
        }
    }

    #[test]
    fn generates_method_per_tool() {
        let tools = vec![
            tool(
                "workspace_read",
                "Read a file from the workspace.",
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "File path"}},
                    "required": ["path"]
                }),
            ),
            tool(
                "workspace_write",
                "Write a file to the workspace.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"]
                }),
            ),
        ];
        let sdk = generate_python_sdk(&tools);
        assert!(sdk.contains("def workspace_read(self, path: str) -> str"));
        assert!(sdk.contains("def workspace_write(self, path: str, content: str) -> str"));
        assert!(sdk.contains("tools = _Tools()"));
        assert!(sdk.contains("\"workspace_read\""));
        assert!(sdk.contains("Read a file from the workspace."));
    }

    #[test]
    fn optional_params_get_default_none() {
        let tools = vec![tool(
            "search_web",
            "Search the web.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["query"]
            }),
        )];
        let sdk = generate_python_sdk(&tools);
        // `query` is required, `limit` is optional (default None)
        assert!(sdk.contains("def search_web(self, query: str, limit: int = None) -> str"));
    }

    #[test]
    fn dashes_in_tool_name_become_underscores() {
        let tools = vec![tool("my-tool", "desc", json!({"type": "object", "properties": {}}))];
        let sdk = generate_python_sdk(&tools);
        assert!(sdk.contains("def my_tool(self) -> str"));
    }

    #[test]
    fn sdk_includes_env_vars_and_call_function() {
        let tools = vec![];
        let sdk = generate_python_sdk(&tools);
        assert!(sdk.contains("OPEX_CODEMODE_TOKEN"));
        assert!(sdk.contains("OPEX_CODEMODE_URL"));
        assert!(sdk.contains("OPEX_SESSION_ID"));
        assert!(sdk.contains("OPEX_AGENT_NAME"));
        assert!(sdk.contains("def _call(tool, arguments):"));
        assert!(sdk.contains("X-Codemode-Token"));
    }

    #[test]
    fn sdk_includes_tools_search() {
        let tools = vec![];
        let sdk = generate_python_sdk(&tools);
        assert!(sdk.contains("def tools_search(query, limit=10):"));
    }

    #[test]
    fn no_properties_yields_no_params() {
        let tools = vec![tool("noop", "No params.", json!({"type": "object"}))];
        let sdk = generate_python_sdk(&tools);
        assert!(sdk.contains("def noop(self) -> str"));
    }

    #[test]
    fn invalid_python_identifier_skipped() {
        // M3: tool names that produce invalid Python identifiers (e.g. with
        // dots or leading digits) must be skipped so they don't break the
        // entire preamble with a SyntaxError.
        let tools = vec![
            tool("valid_tool", "desc", json!({"type": "object", "properties": {}})),
            tool("foo.bar", "dot name", json!({"type": "object", "properties": {}})),
            tool("1tool", "leading digit", json!({"type": "object", "properties": {}})),
            tool("tool bar", "space", json!({"type": "object", "properties": {}})),
        ];
        let sdk = generate_python_sdk(&tools);
        assert!(sdk.contains("def valid_tool(self) -> str"));
        assert!(!sdk.contains("foo.bar"));
        assert!(!sdk.contains("def 1tool"));
        assert!(!sdk.contains("tool bar"));
    }
}