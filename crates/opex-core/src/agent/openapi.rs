//! `OpenAPI` discovery helpers — parse `OpenAPI` 2.x/3.x specs
//! and generate draft YAML tool definitions.

/// Minimal serializable struct for writing draft YAML tool files.
/// Mirrors `YamlToolDef` field names so `serde_yaml` output is compatible.
#[derive(serde::Serialize)]
pub(crate) struct DraftToolYaml {
    pub name: String,
    pub description: String,
    pub endpoint: String,
    pub method: String,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub parameters: std::collections::HashMap<String, DraftParamYaml>,
    pub status: String,
    pub created_by: String,
}

#[derive(serde::Serialize)]
pub(crate) struct DraftParamYaml {
    #[serde(rename = "type")]
    pub param_type: String,
    pub required: bool,
    pub location: String,
    pub description: String,
}

/// Determine server base URL from `OpenAPI` spec.
pub(crate) fn discover_base_url(spec: &serde_json::Value, fallback_url: &str) -> String {
    // OpenAPI 3.x: servers[0].url
    if let Some(url) = spec["servers"][0]["url"].as_str()
        && url.starts_with("http") {
            return url.trim_end_matches('/').to_string();
        }
    // Swagger 2.x: host + basePath
    if let (Some(host), Some(base)) = (spec["host"].as_str(), spec["basePath"].as_str()) {
        let scheme = spec["schemes"][0].as_str().unwrap_or("https");
        return format!("{}://{}{}", scheme, host, base.trim_end_matches('/'));
    }
    // Extract scheme + host from spec URL string
    if let Some(after_scheme) = fallback_url.find("://").map(|i| i + 3) {
        let host_and_rest = &fallback_url[after_scheme..];
        let host = host_and_rest.split('/').next().unwrap_or(host_and_rest);
        let scheme = &fallback_url[..after_scheme - 3];
        return format!("{scheme}://{host}");
    }
    fallback_url.to_string()
}

/// Parse `OpenAPI` 2.x/3.x spec and return draft tool definitions for each operation.
pub(crate) fn extract_openapi_tools(
    spec: &serde_json::Value,
    base_url: &str,
    prefix: &str,
) -> Vec<DraftToolYaml> {
    let paths = match spec["paths"].as_object() {
        Some(p) => p,
        None => return vec![],
    };

    let mut tools = Vec::new();

    for (path, path_item) in paths {
        let path_obj = match path_item.as_object() {
            Some(o) => o,
            None => continue,
        };

        for method in &["get", "post", "put", "patch", "delete"] {
            let operation = match path_obj.get(*method) {
                Some(op) if op.is_object() => op,
                _ => continue,
            };

            // Build tool name from operationId or path+method
            let raw_name = operation["operationId"]
                .as_str()
                .map(std::string::ToString::to_string)
                .unwrap_or_else(|| {
                    let slug = path
                        .trim_matches('/')
                        .replace(['/', '{', '}'], "_")
                        .to_lowercase();
                    format!("{method}_{slug}")
                });

            let name = if prefix.is_empty() {
                sanitize_tool_name(&raw_name)
            } else {
                format!("{}_{}", prefix, sanitize_tool_name(&raw_name))
            };

            let description = operation["summary"]
                .as_str()
                .or_else(|| operation["description"].as_str())
                .unwrap_or(&raw_name)
                .to_string();

            let endpoint = format!("{base_url}{path}");

            // Build parameters map
            let mut parameters = std::collections::HashMap::new();

            // Path/query/header parameters from "parameters" array
            if let Some(params) = operation["parameters"].as_array() {
                for p in params {
                    let pname = match p["name"].as_str() {
                        Some(n) => n.to_string(),
                        None => continue,
                    };
                    let location = p["in"].as_str().unwrap_or("query").to_string();
                    let required = p["required"].as_bool().unwrap_or(location == "path");
                    let description = p["description"].as_str().unwrap_or("").to_string();
                    let param_type = p["schema"]["type"].as_str()
                        .or_else(|| p["type"].as_str())
                        .unwrap_or("string")
                        .to_string();

                    parameters.insert(pname, DraftParamYaml {
                        param_type,
                        required,
                        location,
                        description,
                    });
                }
            }

            // OpenAPI 3.x requestBody → body parameters
            if let Some(body) = operation["requestBody"]["content"]["application/json"]["schema"]
                .as_object()
            {
                let body_props = body.get("properties")
                    .and_then(|v| v.as_object());
                let required_fields: Vec<&str> = body.get("required")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                if let Some(props) = body_props {
                    for (pname, pdef) in props {
                        let param_type = pdef["type"].as_str().unwrap_or("string").to_string();
                        let description = pdef["description"].as_str().unwrap_or("").to_string();
                        let required = required_fields.contains(&pname.as_str());
                        parameters.insert(pname.clone(), DraftParamYaml {
                            param_type,
                            required,
                            location: "body".to_string(),
                            description,
                        });
                    }
                }
            }

            tools.push(DraftToolYaml {
                name,
                description,
                endpoint,
                method: method.to_uppercase(),
                parameters,
                status: "draft".to_string(),
                created_by: "tool_discover".to_string(),
            });
        }
    }

    tools
}

/// Convert an operationId or path slug to a valid `snake_case` tool name.
pub(crate) fn sanitize_tool_name(s: &str) -> String {
    let mut result = String::new();
    let mut prev_underscore = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            // camelCase → snake_case
            if c.is_ascii_uppercase() && !result.is_empty() && !prev_underscore {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
            prev_underscore = false;
        } else if !result.is_empty() && !prev_underscore {
            result.push('_');
            prev_underscore = true;
        }
    }
    result.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sanitize_tool_name ───────────────────────────────────────────────────

    #[test]
    fn sanitize_tool_name_camel_case() {
        assert_eq!(sanitize_tool_name("getUser"), "get_user");
    }

    #[test]
    fn sanitize_tool_name_already_snake() {
        assert_eq!(sanitize_tool_name("already_snake"), "already_snake");
    }

    #[test]
    fn sanitize_tool_name_with_spaces() {
        assert_eq!(sanitize_tool_name("with spaces"), "with_spaces");
    }

    #[test]
    fn sanitize_tool_name_camel_case_method() {
        assert_eq!(sanitize_tool_name("CamelCaseMethod"), "camel_case_method");
    }

    #[test]
    fn sanitize_tool_name_api_path() {
        assert_eq!(sanitize_tool_name("/api/users/{id}"), "api_users_id");
    }

    // ── discover_base_url ────────────────────────────────────────────────────

    #[test]
    fn discover_base_url_openapi3_servers() {
        let spec = serde_json::json!({
            "servers": [{ "url": "https://api.example.com/v1" }]
        });
        assert_eq!(discover_base_url(&spec, "https://fallback.com/spec.json"), "https://api.example.com/v1");
    }

    #[test]
    fn discover_base_url_swagger2_host_basepath() {
        let spec = serde_json::json!({
            "host": "api.example.com",
            "basePath": "/v2/",
            "schemes": ["https"]
        });
        assert_eq!(discover_base_url(&spec, "https://fallback.com/spec.json"), "https://api.example.com/v2");
    }

    #[test]
    fn discover_base_url_fallback_url_extraction() {
        let spec = serde_json::json!({});
        assert_eq!(
            discover_base_url(&spec, "https://api.example.com/openapi.json"),
            "https://api.example.com"
        );
    }

    #[test]
    fn discover_base_url_empty_spec_uses_fallback_host() {
        let spec = serde_json::json!({});
        assert_eq!(
            discover_base_url(&spec, "http://localhost:8080/spec.yaml"),
            "http://localhost:8080"
        );
    }
}
