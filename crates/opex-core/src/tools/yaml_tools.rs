/// YAML-defined HTTP tool registry.
///
/// All tools live flat in `workspace/tools/*.yaml`.
/// Status (verified/draft/disabled) is a field inside each YAML file.
/// Each YAML file defines one tool: endpoint, method, auth, parameters.
/// The registry loads them, converts to JSON Schema for LLM, and executes HTTP calls.
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Trait for resolving environment variable names to values.
/// Falls back to `std::env::var` if no resolver is provided.
#[async_trait]
pub trait EnvResolver: Send + Sync {
    async fn resolve(&self, key: &str) -> Option<String>;
}

/// Resolve an env var: try the resolver first, then fall back to `std::env::var`.
async fn resolve_env(key: &str, resolver: Option<&dyn EnvResolver>) -> Result<String> {
    if let Some(r) = resolver
        && let Some(val) = r.resolve(key).await
    {
        return Ok(val);
    }
    if crate::secrets::is_reserved_secret_name(key) {
        anyhow::bail!("env var '{key}' is reserved and cannot be used as a tool credential");
    }
    std::env::var(key).with_context(|| format!("env var '{key}' not set"))
}
use std::path::Path;
use std::sync::LazyLock;
use tokio::fs;

/// Singleton SSRF-safe HTTP client used to fetch LLM-supplied URLs (e.g.
/// `file_url` parameters in multipart YAML tools). The regular per-engine
/// `http_client` does NOT block private IPs and must never be used for
/// user-controlled URLs. 60s timeout matches the multipart-fetch budget.
fn ssrf_multipart_client() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> =
        LazyLock::new(|| crate::net::ssrf::ssrf_http_client(std::time::Duration::from_secs(60)));
    &CLIENT
}

use opex_types::ToolDefinition;

// ── Status ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    #[default]
    Verified,
    Draft,
    Disabled,
}

// ── Parameter ────────────────────────────────────────────────────────────────

/// Where the parameter is placed in the HTTP request.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ParamLocation {
    Path,
    Query,
    #[default]
    Body,
    Header,
}

#[derive(Debug, Clone, Deserialize)]
pub struct YamlParam {
    #[serde(rename = "type", default = "default_string_type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub location: ParamLocation,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(rename = "enum", default)]
    pub enum_values: Vec<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    #[serde(default)]
    pub examples: Vec<String>,
    /// If the LLM doesn't provide a value, try resolving this env var / scoped secret
    /// before falling back to `default`. Enables per-agent parameter defaults.
    #[serde(default)]
    pub default_from_env: Option<String>,
}

fn default_string_type() -> String {
    "string".to_string()
}

// ── Auth ─────────────────────────────────────────────────────────────────────

/// Authentication configuration for the tool endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct YamlAuth {
    /// `bearer_env` | `bearer_internal` | `basic_env` | `api_key_header` |
    /// `api_key_query` | custom | `oauth_refresh` | `oauth_provider` | none
    ///
    /// `bearer_internal` uses the running core's own auth token and is only
    /// permitted for loopback/internal endpoints (SSRFF-safe). This lets
    /// admin-authored tools call back into Core (`/api/backup`, `/api/config`)
    /// without exposing `OPEX_AUTH_TOKEN` through the generic env resolver,
    /// where it is blocked as a reserved secret name.
    ///
    /// Canonical YAML key is `type`; `auth_type` is accepted as an alias so a
    /// tool authored (e.g. via `tool_create`) with `auth_type:` isn't silently
    /// parsed as no-auth (the whole `auth` block would deserialize to `None`).
    #[serde(rename = "type", alias = "auth_type")]
    pub auth_type: String,
    /// Env var name containing the token/key (or refresh token for `oauth_refresh`).
    pub key: Option<String>,
    /// For `basic_env`: env var for username.
    pub username_key: Option<String>,
    /// For `basic_env`: env var for password.
    pub password_key: Option<String>,
    /// For `api_key_header`: header name (e.g. "X-API-Key").
    pub header_name: Option<String>,
    /// For `api_key_query`: query param name.
    pub param_name: Option<String>,
    /// For custom: map of header → template (${`ENV_VAR`} substituted).
    pub headers: Option<HashMap<String, String>>,
    /// For `oauth_refresh`: token endpoint URL.
    pub token_url: Option<String>,
    /// For `oauth_refresh`: POST body template ({{bearer}} → refresh token).
    pub token_body: Option<String>,
    /// For `oauth_refresh`: JSON field containing the access token (default: "`access_token`").
    pub token_field: Option<String>,
}

// ── Retry config ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct YamlRetryConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_backoff_base")]
    pub backoff_base_ms: u64,
    #[serde(default = "default_retry_on")]
    pub retry_on: Vec<u16>,
}

fn default_max_attempts() -> u32 {
    1
}
fn default_backoff_base() -> u64 {
    1000
}
fn default_retry_on() -> Vec<u16> {
    vec![429, 500, 502, 503, 504]
}

fn default_timeout() -> u64 {
    60
}
fn default_content_type() -> String {
    "application/json".to_string()
}

// ── Cache config ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct YamlCacheConfig {
    /// TTL in seconds.
    pub ttl: u64,
    /// Which parameters form the cache key (empty = all).
    #[serde(default)]
    pub key_params: Vec<String>,
}

// ── Security helpers ─────────────────────────────────────────────────────────

// ERROR_BODY_MAX_CHARS was MOVED to crate::redact alongside redact_secrets (D7).
// The constant is only referenced in #[cfg(test)] — the import lives there.

/// Hard cap on the number of pages the pagination loop will ever fetch,
/// regardless of what `max_pages` is configured to.  Prevents DoS via
/// an artificially large `max_pages` field in a YAML tool definition.
pub(crate) const PAGINATION_MAX_PAGES_HARD_CAP: usize = 1000;

/// Hard cap on the total accumulated size of all paginated pages (in bytes).
/// Prevents DoS via a large `limit` combined with many pages.
pub(crate) const PAGINATION_MAX_TOTAL_BYTES: usize = 50 * 1024 * 1024; // 50 MB

// redact_secrets, redact_pattern_after_keyword, is_token_char, is_token_char_or_separator
// MOVED to crate::redact (D7). Call sites below use crate::redact::redact_secrets directly.

// ── Pagination config ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct YamlPaginationConfig {
    /// offset | cursor | page
    #[serde(rename = "type")]
    pub pagination_type: String,
    /// Query parameter name for offset/cursor/page.
    pub param: String,
    /// Query parameter name for limit.
    pub limit_param: Option<String>,
    /// Items per page.
    pub limit: Option<u32>,
    /// Maximum pages to fetch.
    pub max_pages: Option<u32>,
    /// `JSONPath` to results array in response.
    pub results_path: Option<String>,
    /// `JSONPath` to next cursor value (for cursor pagination).
    pub next_path: Option<String>,
}

// ── Execution context ────────────────────────────────────────────────────────

pub(crate) struct CachedResponse {
    body: String,
    expires_at: std::time::Instant,
}

/// Shared response cache for YAML tools. Process-wide singleton held inside
/// `Arc<ToolExecutionContext>` on `AgentConfig`. Lazy TTL on read, batch
/// eviction on write at the soft cap.
pub struct ToolExecutionContext {
    cache: dashmap::DashMap<String, CachedResponse>,
    max_entries: usize,
}

impl ToolExecutionContext {
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: dashmap::DashMap::new(),
            max_entries,
        }
    }

    /// Test-only inspection.
    #[cfg(test)]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    pub async fn get_cached(&self, key: &str) -> Option<String> {
        let now = std::time::Instant::now();
        let body = {
            let entry = self.cache.get(key)?;
            if now >= entry.expires_at {
                None
            } else {
                Some(entry.body.clone())
            }
        };
        if body.is_none() {
            // Expired — drop the entry.
            self.cache.remove(key);
        }
        body
    }

    pub async fn set_cached(&self, key: &str, body: &str, ttl_secs: u64) {
        if self.cache.len() >= self.max_entries {
            let target_remove = (self.max_entries / 10).max(1);
            let mut victims: Vec<(String, std::time::Instant)> = self
                .cache
                .iter()
                .map(|e| (e.key().clone(), e.value().expires_at))
                .collect();
            victims.sort_by_key(|(_, exp)| *exp);
            for (k, _) in victims.into_iter().take(target_remove) {
                self.cache.remove(&k);
            }
        }
        self.cache.insert(
            key.to_string(),
            CachedResponse {
                body: body.to_string(),
                expires_at: std::time::Instant::now() + std::time::Duration::from_secs(ttl_secs),
            },
        );
    }
}

/// Hard byte cap for buffered YAML-tool text responses (F032). Mirrors the
/// binary path's `MAX_BINARY_SIZE`; the paginated path already caps at
/// [`PAGINATION_MAX_TOTAL_BYTES`].
pub(crate) const MAX_TEXT_RESPONSE_BYTES: usize = 50 * 1024 * 1024; // 50 MB

/// Read a response body as text with a hard byte cap (F032). The non-paginated
/// text path buffered the whole body via `resp.text()` with no limit, so a tool
/// endpoint (agent-authored via tool_create, or reachable through an SSRF gap)
/// returning a multi-GB body OOM-killed the single opex-core process. Streams
/// with a cap and returns a `tool error:` marker (treated as failure downstream)
/// when the body is too large, instead of the previous silent `unwrap_or_default`.
pub(crate) async fn read_body_capped(resp: reqwest::Response) -> String {
    if let Some(len) = resp.content_length()
        && len > MAX_TEXT_RESPONSE_BYTES as u64
    {
        return format!(
            "tool error: response too large ({len} bytes, cap {MAX_TEXT_RESPONSE_BYTES})"
        );
    }
    use futures_util::StreamExt as _;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(c) => {
                if buf.len() + c.len() > MAX_TEXT_RESPONSE_BYTES {
                    return format!(
                        "tool error: response exceeded {MAX_TEXT_RESPONSE_BYTES} byte cap"
                    );
                }
                buf.extend_from_slice(&c);
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

pub(crate) fn build_cache_key(
    agent_name: &str,
    tool_name: &str,
    method: &str,
    endpoint: &str,
    params: &serde_json::Value,
    key_params: &[String],
) -> String {
    // F039: the response cache (tool_exec_ctx) is a process-wide singleton
    // shared by ALL agents, but auth (bearer_env / default_from_env) resolves
    // PER-AGENT scoped secrets — so the same tool+params yields agent-specific
    // responses. Include the agent name so one agent's authenticated private
    // response can't be served to another from the shared cache.
    let mut key = format!("{agent_name}|{tool_name}|{method}|{endpoint}|");
    if let Some(obj) = params.as_object() {
        if key_params.is_empty() {
            // serde_json::Map preserves insertion order (preserve_order feature
            // enabled via schemars). Sort keys explicitly so the cache key is
            // independent of JSON-object key order.
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for k in keys {
                if let Some(v) = obj.get(k) {
                    key.push_str(k);
                    key.push('=');
                    key.push_str(&v.to_string());
                    key.push('&');
                }
            }
        } else {
            // key_params provides explicit order — preserve it.
            for kp in key_params {
                if let Some(v) = obj.get(kp) {
                    key.push_str(kp);
                    key.push('=');
                    key.push_str(&v.to_string());
                    key.push('&');
                }
            }
        }
    }
    key
}

// ── GraphQL config ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct YamlGraphqlConfig {
    /// The GraphQL query string.
    pub query: String,
    /// Variable templates with {{param}} substitution.
    pub variables: Option<HashMap<String, String>>,
}

// ── Response pipeline ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ResponsePipelineStep {
    Jsonpath(String),
    PickFields(Vec<String>),
    SortBy { field: String, desc: bool },
    Limit(usize),
    /// Join an array of scalars into a single string with the given separator.
    Join(String),
}

/// Intermediate struct for YAML deserialization of pipeline steps.
/// Each step is a map with exactly one key.
#[derive(Deserialize)]
struct RawPipelineStep {
    jsonpath: Option<String>,
    pick_fields: Option<Vec<String>>,
    sort_by: Option<RawSortBy>,
    limit: Option<usize>,
    join: Option<String>,
}

#[derive(Deserialize)]
struct RawSortBy {
    field: String,
    #[serde(default)]
    desc: bool,
}

impl<'de> Deserialize<'de> for ResponsePipelineStep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawPipelineStep::deserialize(deserializer)?;
        if let Some(path) = raw.jsonpath {
            Ok(ResponsePipelineStep::Jsonpath(path))
        } else if let Some(fields) = raw.pick_fields {
            Ok(ResponsePipelineStep::PickFields(fields))
        } else if let Some(sort) = raw.sort_by {
            Ok(ResponsePipelineStep::SortBy {
                field: sort.field,
                desc: sort.desc,
            })
        } else if let Some(count) = raw.limit {
            Ok(ResponsePipelineStep::Limit(count))
        } else if let Some(sep) = raw.join {
            Ok(ResponsePipelineStep::Join(sep))
        } else {
            Err(serde::de::Error::custom(
                "pipeline step must have exactly one key: jsonpath, pick_fields, sort_by, limit, or join",
            ))
        }
    }
}

fn apply_pipeline(
    value: serde_json::Value,
    pipeline: &[ResponsePipelineStep],
) -> serde_json::Value {
    let mut current = value;
    for step in pipeline {
        current = match step {
            ResponsePipelineStep::Jsonpath(path) => {
                apply_jsonpath(&current, path).unwrap_or(current)
            }
            ResponsePipelineStep::PickFields(fields) => {
                if let Some(arr) = current.as_array() {
                    let filtered: Vec<serde_json::Value> = arr
                        .iter()
                        .map(|item| {
                            if let Some(obj) = item.as_object() {
                                let picked: serde_json::Map<String, serde_json::Value> = obj
                                    .iter()
                                    .filter(|(k, _)| fields.contains(k))
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect();
                                serde_json::Value::Object(picked)
                            } else {
                                item.clone()
                            }
                        })
                        .collect();
                    serde_json::Value::Array(filtered)
                } else {
                    current
                }
            }
            ResponsePipelineStep::SortBy { field, desc } => {
                if let Some(arr) = current.as_array() {
                    let mut sorted = arr.clone();
                    sorted.sort_by(|a, b| {
                        let va = a
                            .get(field)
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.0);
                        let vb = b
                            .get(field)
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.0);
                        if *desc {
                            vb.partial_cmp(&va).unwrap_or(std::cmp::Ordering::Equal)
                        } else {
                            va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
                        }
                    });
                    serde_json::Value::Array(sorted)
                } else {
                    current
                }
            }
            ResponsePipelineStep::Limit(count) => {
                if let Some(arr) = current.as_array() {
                    serde_json::Value::Array(arr.iter().take(*count).cloned().collect())
                } else {
                    current
                }
            }
            ResponsePipelineStep::Join(sep) => {
                if let Some(arr) = current.as_array() {
                    // Join scalar elements; a string stays verbatim, numbers/bools
                    // stringify, and anything non-scalar is JSON-encoded so nothing
                    // is silently dropped.
                    let joined = arr
                        .iter()
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(sep);
                    serde_json::Value::String(joined)
                } else {
                    current
                }
            }
        };
    }
    current
}

// ── Channel action ────────────────────────────────────────────────────────────

/// After a successful HTTP call, instruct the engine to perform a channel action
/// using the binary response body (e.g. send TTS audio as a Telegram voice message).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // data_field is parsed from YAML for forward-compat with planned channel actions.
pub struct ChannelActionConfig {
    /// Action name: "`send_voice`", "`send_file`", etc.
    pub action: String,
    /// Where to take the data from:
    /// - "_binary" — use the raw binary response body
    /// - "$.field"  — extract a JSON field from the response
    pub data_field: String,
}

// ── Tool definition ──────────────────────────────────────────────────────────

/// Full YAML tool definition loaded from a file.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // YAML schema fields (extends/created_by/cache) are accepted on
// parse for forward-compat; runtime enforcement lives elsewhere.
pub struct YamlToolDef {
    #[serde(default)]
    pub extends: Option<String>,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub endpoint: String,
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub parameters: HashMap<String, YamlParam>,
    pub auth: Option<YamlAuth>,
    /// Optional Mustache-style body template with {{param}} substitution.
    pub body_template: Option<String>,
    /// Optional `JSONPath` expression to extract a sub-value from the response.
    /// Example: "$.data.items" extracts items array from {"data":{"items":[...]}}.
    pub response_transform: Option<String>,
    /// If set, after a successful HTTP call the engine performs a channel action
    /// (e.g. sends binary audio as a Telegram voice message).
    pub channel_action: Option<ChannelActionConfig>,
    #[serde(default)]
    pub status: ToolStatus,
    #[serde(default)]
    pub created_by: String,
    /// Per-tool timeout in seconds (default 60).
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Retry configuration for transient failures.
    pub retry: Option<YamlRetryConfig>,
    /// Content-Type for request body (default: application/json).
    #[serde(default = "default_content_type")]
    pub content_type: String,
    /// Response caching configuration.
    pub cache: Option<YamlCacheConfig>,
    /// Pagination configuration for auto-fetching multiple pages.
    pub pagination: Option<YamlPaginationConfig>,
    /// Response schema hint for LLM (appended to description).
    pub response_schema: Option<serde_json::Value>,
    /// GraphQL query configuration (overrides `body_template`).
    pub graphql: Option<YamlGraphqlConfig>,
    /// Response processing pipeline (applied after `response_transform`).
    #[serde(default)]
    pub response_pipeline: Vec<ResponsePipelineStep>,
    /// If true, this tool is only available to base (system) agents.
    #[serde(default)]
    pub required_base: bool,
    /// If true, this (admin-authored) tool may target a private RFC1918 LAN /
    /// tunnel address. Its requests use the LAN client, which still blocks
    /// loopback, cloud-metadata/link-local, and CGNAT — but permits 10/8,
    /// 172.16/12, 192.168/16 (e.g. a home-lab Home Assistant over WireGuard).
    /// Pair with `required_base: true` so only trusted agents can reach it.
    #[serde(default)]
    pub allow_private_endpoint: bool,
    /// If true, this tool is safe for concurrent execution with other parallel-safe tools.
    #[serde(default)]
    pub parallel: bool,
    /// Secrets required by internal toolgate routers (not covered by auth.key).
    #[serde(default)]
    pub required_secrets: Vec<String>,
}

/// Context for resolving OAuth provider tokens during YAML tool execution.
/// Bridges `OAuthManager::get_token()` into the YAML tool auth pipeline.
pub struct OAuthContext {
    pub manager: std::sync::Arc<crate::oauth::OAuthManager>,
    pub agent_id: String,
}

impl YamlToolDef {
    /// Convert to `ToolDefinition` (JSON Schema) for the LLM.
    pub fn to_tool_definition(&self) -> ToolDefinition {
        let mut properties = serde_json::Map::new();
        let mut required_fields = Vec::new();

        for (param_name, param) in &self.parameters {
            let mut prop = serde_json::Map::new();
            prop.insert(
                "type".into(),
                serde_json::Value::String(param.param_type.clone()),
            );

            let mut desc = param.description.clone();
            if !param.examples.is_empty() {
                desc.push_str(&format!(" Examples: {}", param.examples.join(", ")));
            }
            prop.insert("description".into(), serde_json::Value::String(desc));

            if !param.enum_values.is_empty() {
                prop.insert(
                    "enum".into(),
                    serde_json::Value::Array(
                        param
                            .enum_values
                            .iter()
                            .map(|v| serde_json::Value::String(v.clone()))
                            .collect(),
                    ),
                );
            }
            if let Some(ref default) = param.default {
                prop.insert("default".into(), default.clone());
            }
            if let Some(min) = param.minimum {
                prop.insert(
                    "minimum".into(),
                    serde_json::Value::Number(
                        serde_json::Number::from_f64(min).unwrap_or(serde_json::Number::from(0)),
                    ),
                );
            }
            if let Some(max) = param.maximum {
                prop.insert(
                    "maximum".into(),
                    serde_json::Value::Number(
                        serde_json::Number::from_f64(max).unwrap_or(serde_json::Number::from(0)),
                    ),
                );
            }

            properties.insert(param_name.clone(), serde_json::Value::Object(prop));

            if param.required {
                required_fields.push(param_name.clone());
            }
        }

        let schema = serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required_fields,
        });

        let mut description = self.description.clone();
        // Show required secret names so agents know what to save
        if let Some(ref auth) = self.auth
            && let Some(ref key) = auth.key
        {
            description.push_str(&format!(" [requires secret: {key}]"));
        }
        for secret in &self.required_secrets {
            description.push_str(&format!(" [requires secret: {secret}]"));
        }
        if let Some(ref rs) = self.response_schema
            && let Ok(pretty) = serde_json::to_string_pretty(rs)
        {
            description.push_str("\n\nResponse schema: ");
            description.push_str(&pretty);
        }

        ToolDefinition {
            name: self.name.clone(),
            description,
            input_schema: schema,
        }
    }

    /// Build and send the HTTP request, returning the raw response.
    async fn send_request(
        &self,
        params: &serde_json::Value,
        http_client: &reqwest::Client,
        env_resolver: Option<&dyn EnvResolver>,
        oauth_context: Option<&OAuthContext>,
        injected_headers: &[(String, String)],
    ) -> Result<reqwest::Response> {
        let params_map = params.as_object().cloned().unwrap_or_default();

        // 0. Validate required parameters
        for (name, param) in &self.parameters {
            let val = params_map.get(name);
            if param.required && (val.is_none() || val == Some(&serde_json::Value::Null)) {
                // Check if default_from_env or default can fill it
                let has_env_default = if let Some(env_key) = &param.default_from_env {
                    let from_resolver = if let Some(r) = env_resolver {
                        r.resolve(env_key).await.is_some()
                    } else {
                        false
                    };
                    from_resolver || std::env::var(env_key).is_ok()
                } else {
                    false
                };
                let has_default = param.default.is_some();
                if !has_env_default && !has_default {
                    anyhow::bail!("required parameter '{name}' is missing");
                }
            }
            if let Some(v) = val
                && param.param_type == "integer"
                && !v.is_number()
                && !v.is_null()
            {
                anyhow::bail!("parameter '{name}' must be integer, got {v}");
            }
        }

        // 1. Build URL with path parameter substitution
        let mut url = self.endpoint.clone();
        let mut query_params: Vec<(String, String)> = Vec::new();
        let mut extra_headers: Vec<(String, String)> = Vec::new();
        let mut body_params: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

        for (name, param) in &self.parameters {
            // Resolution order: LLM arg → default_from_env (scoped secret) → default
            let value = if let Some(v) = params_map.get(name).cloned() {
                v
            } else if let Some(ref env_key) = param.default_from_env {
                if let Some(resolver) = env_resolver {
                    if let Some(val) = resolver.resolve(env_key).await {
                        serde_json::Value::String(val)
                    } else {
                        param.default.clone().unwrap_or(serde_json::Value::Null)
                    }
                } else {
                    param.default.clone().unwrap_or(serde_json::Value::Null)
                }
            } else {
                param.default.clone().unwrap_or(serde_json::Value::Null)
            };

            if value.is_null() {
                continue;
            }

            let value_str = match &value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            match param.location {
                ParamLocation::Path => {
                    url = url.replace(&format!("{{{name}}}"), &urlencoding::encode(&value_str));
                }
                ParamLocation::Query => {
                    query_params.push((name.clone(), value_str));
                }
                ParamLocation::Header => {
                    extra_headers.push((name.clone(), value_str));
                }
                ParamLocation::Body => {
                    body_params.insert(name.clone(), value);
                }
            }
        }

        // 2. Apply auth
        let mut auth_headers: Vec<(String, String)> = Vec::new();
        let mut auth_query: Vec<(String, String)> = Vec::new();

        if let Some(ref auth) = self.auth {
            match auth.auth_type.as_str() {
                "bearer_env" => {
                    if let Some(ref key) = auth.key {
                        let token = resolve_env(key, env_resolver).await?;
                        auth_headers.push(("Authorization".into(), format!("Bearer {token}")));
                    }
                }
                "bearer_internal" => {
                    // Use the running core's own Bearer token for callbacks to
                    // localhost/internal endpoints. Security gate: only allowed
                    // for endpoints that are already trusted (loopback or admin-
                    // configured internal services) so the token cannot be
                    // exfiltrated to arbitrary external URLs.
                    if !crate::tools::ssrf::is_internal_endpoint(&url) {
                        anyhow::bail!(
                            "auth type 'bearer_internal' is only permitted for loopback/internal endpoints; \
                             endpoint '{}' is not internal",
                            url
                        );
                    }
                    let token = crate::gateway::shared_token()
                        .ok_or_else(|| anyhow::anyhow!("core shared token not available"))?;
                    // Audit trail: a bearer_internal call forwards the core's
                    // own auth token to an internal endpoint. Logged at INFO
                    // (not DEBUG) so it is visible in the default journalctl
                    // stream — this is a sensitive operation that should be
                    // traceable after the fact.
                    tracing::info!(
                        tool = %self.name,
                        endpoint = %url,
                        "bearer_internal: forwarding core auth token to internal endpoint"
                    );
                    auth_headers.push(("Authorization".into(), format!("Bearer {token}")));
                }
                "basic_env" => {
                    let user = auth.username_key.as_deref().unwrap_or("");
                    let pass = auth.password_key.as_deref().unwrap_or("");
                    let user_val = resolve_env(user, env_resolver).await.unwrap_or_default();
                    let pass_val = resolve_env(pass, env_resolver).await.unwrap_or_default();
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{user_val}:{pass_val}"));
                    auth_headers.push(("Authorization".into(), format!("Basic {encoded}")));
                }
                "api_key_header" => {
                    if let (Some(hdr), Some(key)) = (&auth.header_name, &auth.key) {
                        let val = resolve_env(key, env_resolver).await?;
                        auth_headers.push((hdr.clone(), val));
                    }
                }
                "api_key_query" => {
                    if let (Some(param), Some(key)) = (&auth.param_name, &auth.key) {
                        let val = resolve_env(key, env_resolver).await?;
                        auth_query.push((param.clone(), val));
                    }
                }
                "custom" => {
                    if let Some(ref hdrs) = auth.headers {
                        for (hdr_name, tpl) in hdrs {
                            let resolved = resolve_env_template(tpl, env_resolver).await;
                            auth_headers.push((hdr_name.clone(), resolved));
                        }
                    }
                }
                "oauth_refresh" => {
                    if let (Some(key), Some(token_url)) = (&auth.key, &auth.token_url) {
                        let refresh_token = resolve_env(key, env_resolver).await?;
                        let body = auth
                            .token_body
                            .as_deref()
                            .unwrap_or("grant_type=refresh_token&refresh_token={{bearer}}")
                            .replace("{{bearer}}", &refresh_token);
                        let token_field = auth.token_field.as_deref().unwrap_or("access_token");

                        // SSRF protection: validate token_url before fetching
                        crate::tools::ssrf::validate_url_scheme(token_url)?;

                        let resp = http_client
                            .post(token_url)
                            .header("Content-Type", "application/x-www-form-urlencoded")
                            .body(body)
                            .send()
                            .await
                            .map_err(|e| anyhow::anyhow!("oauth token request failed: {e}"))?;

                        if !resp.status().is_success() {
                            let status = resp.status();
                            let body = read_body_capped(resp).await;
                            // Bug 8: redact secrets from OAuth error body before logging
                            anyhow::bail!(
                                "oauth token endpoint returned {status}: {}",
                                crate::redact::redact_secrets(&body)
                            );
                        }

                        let json: serde_json::Value = resp
                            .json()
                            .await
                            .map_err(|e| anyhow::anyhow!("oauth token response not JSON: {e}"))?;
                        let access_token = json
                            .get(token_field)
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                anyhow::anyhow!("oauth response missing '{token_field}' field")
                            })?;
                        auth_headers
                            .push(("Authorization".into(), format!("Bearer {access_token}")));
                    }
                }
                "oauth_provider" => {
                    let provider = auth.key.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("oauth_provider auth requires 'key' field (provider name)")
                    })?;
                    let ctx = oauth_context
                        .ok_or_else(|| anyhow::anyhow!("oauth_provider auth for '{provider}' requires OAuth connection — connect via /integrations"))?;
                    let token = ctx
                        .manager
                        .get_token(provider, &ctx.agent_id)
                        .await
                        .map_err(|e| anyhow::anyhow!("OAuth token for {provider}: {e}"))?;
                    auth_headers.push(("Authorization".into(), format!("Bearer {token}")));
                }
                _ => {} // "none" or unknown — no auth
            }
        }

        // 3. Build request
        let method = self.method.to_uppercase();
        let mut builder = match method.as_str() {
            "GET" => http_client.get(&url),
            "POST" => http_client.post(&url),
            "PUT" => http_client.put(&url),
            "PATCH" => http_client.patch(&url),
            "DELETE" => http_client.delete(&url),
            other => anyhow::bail!("unsupported HTTP method: {other}"),
        };

        // Static headers from tool definition
        for (k, v) in &self.headers {
            builder = builder.header(k, v);
        }
        // Dynamic headers (from params + auth)
        for (k, v) in auth_headers {
            builder = builder.header(k, v);
        }
        // Caller-injected headers (e.g. X-Opex-Provider for per-agent TTS routing).
        // Applied LAST so they take precedence over anything declared in the YAML def.
        for (k, v) in injected_headers {
            builder = builder.header(k, v);
        }
        for (k, v) in extra_headers {
            builder = builder.header(k, v);
        }

        // Query params
        let all_query: Vec<_> = query_params.into_iter().chain(auth_query).collect();
        if !all_query.is_empty() {
            builder = builder.query(&all_query);
        }

        // Body: GraphQL takes priority, then body_template, then body params
        if method != "GET" && method != "DELETE" {
            if let Some(ref gql) = self.graphql {
                let mut vars = serde_json::Map::new();
                if let Some(ref var_templates) = gql.variables {
                    for (k, tpl) in var_templates {
                        // Substitute {{param}} in variable templates
                        let mut val = tpl.clone();
                        for (name, pv) in &params_map {
                            let pv_str = match pv {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            val = val.replace(&format!("{{{{{name}}}}}"), &pv_str);
                        }
                        vars.insert(k.clone(), serde_json::Value::String(val));
                    }
                }
                let gql_body = serde_json::json!({
                    "query": gql.query,
                    "variables": vars
                });
                builder = builder
                    .header("Content-Type", "application/json")
                    .body(gql_body.to_string());
            } else if let Some(ref template) = self.body_template {
                // `render_body_template` JSON-escapes substitutions (\, ", \n,
                // \r, \t) but leaves `&`, `=`, `?` untouched. That is correct
                // for application/json but unsafe for x-www-form-urlencoded /
                // text/plain — a parameter value like `value&admin=1` would
                // inject extra form fields. Refuse the combination so a
                // future YAML tool config cannot be silently exploited.
                let ct = self.content_type.to_ascii_lowercase();
                if !(ct.starts_with("application/json")
                    || ct.starts_with("application/vnd.api+json")
                    || ct.starts_with("application/ld+json"))
                {
                    anyhow::bail!(
                        "tool '{}': body_template requires a JSON content_type \
                         (got '{}'); use body params or a JSON content_type to avoid \
                         template-injection across delimiter characters",
                        self.name,
                        self.content_type
                    );
                }
                let body = render_body_template(template, &params_map, env_resolver).await;
                builder = builder
                    .header("Content-Type", &self.content_type)
                    .body(body);
            } else if !body_params.is_empty() {
                if self.content_type == "multipart/form-data" {
                    let mut form = reqwest::multipart::Form::new();
                    for (name, val) in &body_params {
                        let val_str = match val {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        if name == "file" || name.ends_with("_url") {
                            // The URL here is LLM-controlled. Use the SSRF-safe
                            // client with private-IP DNS filtering and reject
                            // schemes outside http(s); the regular http_client
                            // would otherwise let an agent fetch
                            // http://169.254.169.254/... or any internal
                            // service.
                            crate::net::ssrf::validate_url_scheme(&val_str).map_err(|e| {
                                anyhow::anyhow!(
                                    "rejected file URL '{val_str}' for multipart upload: {e}"
                                )
                            })?;
                            let ssrf_client = ssrf_multipart_client();
                            let dl_resp = ssrf_client
                                .get(&val_str)
                                .send()
                                .await
                                .context("failed to download file for multipart")?;
                            // F032: cap the LLM-controlled file download so it
                            // can't OOM core with a multi-GB body.
                            if let Some(len) = dl_resp.content_length()
                                && len > MAX_TEXT_RESPONSE_BYTES as u64
                            {
                                anyhow::bail!(
                                    "multipart file too large: {len} bytes (cap {MAX_TEXT_RESPONSE_BYTES})"
                                );
                            }
                            let bytes = dl_resp
                                .bytes()
                                .await
                                .context("failed to read file bytes")?;
                            if bytes.len() > MAX_TEXT_RESPONSE_BYTES {
                                anyhow::bail!(
                                    "multipart file exceeded {MAX_TEXT_RESPONSE_BYTES} byte cap"
                                );
                            }
                            let filename = body_params
                                .get("file_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("file")
                                .to_string();
                            let part =
                                reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(filename);
                            form = form.part(name.clone(), part);
                        } else {
                            form = form.text(name.clone(), val_str);
                        }
                    }
                    builder = builder.multipart(form);
                } else if self.content_type == "application/x-www-form-urlencoded" {
                    let form_body: Vec<(String, String)> = body_params
                        .iter()
                        .map(|(k, v)| {
                            let val_str = match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            (k.clone(), val_str)
                        })
                        .collect();
                    builder = builder.form(&form_body);
                } else {
                    builder = builder.json(&serde_json::Value::Object(body_params));
                }
            }
        }

        // Apply per-tool timeout
        tokio::time::timeout(std::time::Duration::from_secs(self.timeout), builder.send())
            .await
            .map_err(|_| anyhow::anyhow!("tool '{}' timed out after {}s", self.name, self.timeout))?
            .context("HTTP request failed")
    }

    /// Max retry attempts (from config or default 1 = no retry).
    fn max_attempts(&self) -> u32 {
        self.retry.as_ref().map_or(1, |r| r.max_attempts)
    }

    /// Check if status code is retryable.
    fn is_retryable(&self, status: u16) -> bool {
        self.retry
            .as_ref()
            .is_some_and(|r| r.retry_on.contains(&status))
    }

    /// Backoff base in ms.
    fn backoff_base_ms(&self) -> u64 {
        self.retry.as_ref().map_or(1000, |r| r.backoff_base_ms)
    }

    /// Execute with OAuth context for provider-based auth.
    pub async fn execute_oauth(
        &self,
        params: &serde_json::Value,
        http_client: &reqwest::Client,
        env_resolver: Option<&dyn EnvResolver>,
        oauth_context: Option<&OAuthContext>,
    ) -> Result<String> {
        self.execute_with_ctx(params, http_client, env_resolver, oauth_context, &[])
            .await
    }

    /// Execute the tool, injecting OAuth bearer credentials when `oauth_context` is provided.
    /// `injected_headers` are appended to the outgoing request (used by callers such as
    /// `channel_actions::execute_yaml_channel_action` to set per-agent routing headers).
    pub async fn execute_with_ctx(
        &self,
        params: &serde_json::Value,
        http_client: &reqwest::Client,
        env_resolver: Option<&dyn EnvResolver>,
        oauth_context: Option<&OAuthContext>,
        injected_headers: &[(String, String)],
    ) -> Result<String> {
        // Pagination: auto-fetch multiple pages if configured
        if let Some(ref pagination) = self.pagination {
            return self
                .execute_paginated(
                    params,
                    http_client,
                    env_resolver,
                    pagination,
                    oauth_context,
                    injected_headers,
                )
                .await;
        }

        let start = std::time::Instant::now();
        let max = self.max_attempts();
        let mut last_err = None;

        for attempt in 0..max {
            if attempt > 0 {
                let delay = self
                    .backoff_base_ms()
                    .saturating_mul(2u64.saturating_pow(attempt.min(63) - 1));
                tracing::warn!(tool = %self.name, attempt, delay_ms = delay, "retrying yaml tool");
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }

            let resp = match self
                .send_request(
                    params,
                    http_client,
                    env_resolver,
                    oauth_context,
                    injected_headers,
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < max {
                        continue;
                    }
                    break;
                }
            };
            let status = resp.status();
            let body = read_body_capped(resp).await;

            if status.is_success() {
                let elapsed = start.elapsed();
                tracing::info!(
                    tool = %self.name, status = %status,
                    elapsed_ms = elapsed.as_millis() as u64,
                    attempt = attempt + 1, "yaml tool executed"
                );

                // Apply response_transform (JSONPath) then response_pipeline
                let mut result_body = body;
                if let Some(ref path) = self.response_transform
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(&result_body)
                    && let Some(extracted) = apply_jsonpath(&json, path)
                {
                    result_body = match extracted {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                }

                // Apply response pipeline if configured
                if !self.response_pipeline.is_empty()
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(&result_body)
                {
                    let transformed = apply_pipeline(json, &self.response_pipeline);
                    result_body = match transformed {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                }

                return Ok(result_body);
            }

            // Check if retryable
            if attempt + 1 < max && self.is_retryable(status.as_u16()) {
                // Bug 10: redact secrets from audit/retry error bodies
                last_err = Some(anyhow::anyhow!(
                    "tool '{}' returned HTTP {}: {}",
                    self.name,
                    status,
                    crate::redact::redact_secrets(&body)
                ));
                continue;
            }

            // Bug 7: redact secrets from error response bodies
            anyhow::bail!(
                "tool '{}' returned HTTP {}: {}",
                self.name,
                status,
                crate::redact::redact_secrets(&body)
            );
        }

        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("tool '{}' failed after {} attempts", self.name, max)
        }))
    }

    /// Execute with automatic pagination.
    async fn execute_paginated(
        &self,
        params: &serde_json::Value,
        http_client: &reqwest::Client,
        env_resolver: Option<&dyn EnvResolver>,
        pagination: &YamlPaginationConfig,
        oauth_context: Option<&OAuthContext>,
        injected_headers: &[(String, String)],
    ) -> Result<String> {
        let mut all_results: Vec<serde_json::Value> = Vec::new();

        // Bug 9: clamp max_pages to the hard cap; warn if the config exceeds it.
        let configured_max_pages = pagination.max_pages.unwrap_or(5) as usize;
        let max_pages = if configured_max_pages > PAGINATION_MAX_PAGES_HARD_CAP {
            tracing::warn!(
                tool = %self.name,
                configured = configured_max_pages,
                capped = PAGINATION_MAX_PAGES_HARD_CAP,
                "pagination max_pages exceeds hard cap; clamping"
            );
            PAGINATION_MAX_PAGES_HARD_CAP
        } else {
            configured_max_pages
        };

        let limit = pagination.limit.unwrap_or(50);
        let mut cursor: Option<String> = None;
        // Bug 9: track accumulated response size to enforce the total-bytes cap.
        let mut total_bytes: usize = 0;

        for page in 0..max_pages {
            let mut page_params = params.clone();
            if let Some(obj) = page_params.as_object_mut() {
                match pagination.pagination_type.as_str() {
                    "offset" => {
                        // Bug 21: use u64 arithmetic to avoid u32 overflow when
                        // page * limit exceeds 2^32.  After the hard-cap clamp
                        // (≤1000) this is unreachable in practice, but kept for
                        // defence-in-depth.
                        let offset = (page as u64).saturating_mul(u64::from(limit));
                        obj.insert(pagination.param.clone(), serde_json::json!(offset));
                    }
                    "page" => {
                        obj.insert(pagination.param.clone(), serde_json::json!(page as u64 + 1));
                    }
                    "cursor" => {
                        if let Some(ref c) = cursor {
                            obj.insert(
                                pagination.param.clone(),
                                serde_json::Value::String(c.clone()),
                            );
                        } else if page > 0 {
                            break; // No next cursor
                        }
                    }
                    _ => {}
                }
                if let Some(ref lp) = pagination.limit_param {
                    obj.insert(lp.clone(), serde_json::json!(limit));
                }
            }

            // Use a clone without pagination to avoid recursion
            let body = self
                .execute_single(
                    &page_params,
                    http_client,
                    env_resolver,
                    oauth_context,
                    injected_headers,
                )
                .await?;

            // Bug 9: enforce per-page size contribution to the total-bytes cap.
            total_bytes += body.len();
            if total_bytes > PAGINATION_MAX_TOTAL_BYTES {
                tracing::warn!(
                    tool = %self.name,
                    page,
                    total_bytes,
                    cap = PAGINATION_MAX_TOTAL_BYTES,
                    "pagination total size cap reached; returning partial results"
                );
                // Still parse and include the current page up to the cap, then stop.
                let json: serde_json::Value =
                    serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body));
                let items = if let Some(ref rp) = pagination.results_path {
                    apply_jsonpath(&json, rp).unwrap_or(json.clone())
                } else {
                    json.clone()
                };
                let items_arr = items.as_array().cloned().unwrap_or_else(|| vec![items]);
                all_results.extend(items_arr);
                break;
            }

            // Extract results
            let json: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body));
            let items = if let Some(ref rp) = pagination.results_path {
                apply_jsonpath(&json, rp).unwrap_or(json.clone())
            } else {
                json.clone()
            };

            let items_arr = items.as_array().cloned().unwrap_or_else(|| vec![items]);
            if items_arr.is_empty() {
                break;
            }
            all_results.extend(items_arr);

            // Extract next cursor
            if pagination.pagination_type == "cursor" {
                cursor = pagination
                    .next_path
                    .as_ref()
                    .and_then(|np| apply_jsonpath(&json, np))
                    .and_then(|v| v.as_str().map(std::string::ToString::to_string));
                if cursor.is_none() {
                    break;
                }
            }
        }

        let result = serde_json::to_string(&all_results)?;
        Ok(result)
    }

    /// Execute a single HTTP call without pagination (used by `execute_paginated`).
    async fn execute_single(
        &self,
        params: &serde_json::Value,
        http_client: &reqwest::Client,
        env_resolver: Option<&dyn EnvResolver>,
        oauth_context: Option<&OAuthContext>,
        injected_headers: &[(String, String)],
    ) -> Result<String> {
        let max = self.max_attempts();
        let mut last_err = None;

        for attempt in 0..max {
            if attempt > 0 {
                let delay = self
                    .backoff_base_ms()
                    .saturating_mul(2u64.saturating_pow(attempt.min(63) - 1));
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }

            let resp = match self
                .send_request(
                    params,
                    http_client,
                    env_resolver,
                    oauth_context,
                    injected_headers,
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < max {
                        continue;
                    }
                    break;
                }
            };
            let status = resp.status();
            let body = read_body_capped(resp).await;

            if status.is_success() {
                return Ok(body);
            }
            if attempt + 1 < max && self.is_retryable(status.as_u16()) {
                // Bug 10: redact secrets from error bodies surfaced by pagination sub-calls
                last_err = Some(anyhow::anyhow!(
                    "HTTP {status}: {}",
                    crate::redact::redact_secrets(&body)
                ));
                continue;
            }
            // Bug 7: redact secrets from error response bodies
            anyhow::bail!(
                "tool '{}' returned HTTP {}: {}",
                self.name,
                status,
                crate::redact::redact_secrets(&body)
            );
        }

        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("tool '{}' failed after {} attempts", self.name, max)
        }))
    }

    /// Execute this tool and return the raw binary response body.
    /// Used by the engine for `channel_action` tools (e.g. TTS → `send_voice`).
    pub async fn execute_binary(
        &self,
        params: &serde_json::Value,
        http_client: &reqwest::Client,
        env_resolver: Option<&dyn EnvResolver>,
        oauth_context: Option<&OAuthContext>,
        injected_headers: &[(String, String)],
    ) -> Result<Vec<u8>> {
        let max = self.max_attempts();
        let mut last_err = None;

        for attempt in 0..max {
            if attempt > 0 {
                let delay = self
                    .backoff_base_ms()
                    .saturating_mul(2u64.saturating_pow(attempt.min(63) - 1));
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }

            let resp = match self
                .send_request(
                    params,
                    http_client,
                    env_resolver,
                    oauth_context,
                    injected_headers,
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < max {
                        continue;
                    }
                    break;
                }
            };
            let status = resp.status();

            if status.is_success() {
                const MAX_BINARY_SIZE: usize = 50 * 1024 * 1024; // 50MB
                if let Some(cl) = resp.content_length()
                    && cl > MAX_BINARY_SIZE as u64
                {
                    anyhow::bail!("response too large: {cl} bytes (max {MAX_BINARY_SIZE})");
                }
                let bytes = resp
                    .bytes()
                    .await
                    .context("failed to read response bytes")?;
                if bytes.len() > MAX_BINARY_SIZE {
                    anyhow::bail!(
                        "binary response too large: {} bytes (max {})",
                        bytes.len(),
                        MAX_BINARY_SIZE
                    );
                }
                return Ok(bytes.to_vec());
            }

            let body = read_body_capped(resp).await;
            if attempt + 1 < max && self.is_retryable(status.as_u16()) {
                // Bug 10: redact secrets from binary-tool error bodies
                last_err = Some(anyhow::anyhow!(
                    "tool '{}' returned HTTP {}: {}",
                    self.name,
                    status,
                    crate::redact::redact_secrets(&body)
                ));
                continue;
            }

            // Bug 7: redact secrets from binary-tool error response bodies
            anyhow::bail!(
                "tool '{}' returned HTTP {}: {}",
                self.name,
                status,
                crate::redact::redact_secrets(&body)
            );
        }

        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!("tool '{}' failed after {} attempts", self.name, max)
        }))
    }
}

/// `JSONPath` resolver supporting "$.key", "$.key.nested", "$.arr[0]", "$.arr[*]", "$.arr[-1]", "$.arr[0:3]".
/// One step of a parsed JSONPath: an object key, an array index (possibly
/// negative), a `[start:end]` slice, or a `[*]` wildcard projection.
#[derive(Debug, Clone)]
enum PathToken {
    Key(String),
    Index(isize),
    Slice(Option<usize>, Option<usize>),
    Wildcard,
}

/// Tokenize a `$`-stripped path into [`PathToken`]s. Supports keys (`a.b`),
/// consecutive brackets in one hop (`[0][0][0]` — Google-Translate-style nested
/// arrays), `[*]`, `[-1]`, and `[start:end]`. Returns `None` on a malformed
/// bracket so the caller falls back to the untransformed value.
// reviewed: every slice offset comes from find(']' / '.' / '[') or strip_prefix
// on ASCII delimiters, so it always lands on a char boundary — char-safe.
#[allow(clippy::string_slice)]
fn tokenize_jsonpath(path: &str) -> Option<Vec<PathToken>> {
    let mut toks = Vec::new();
    let mut rest = path;
    loop {
        rest = rest.trim_start_matches('.');
        if rest.is_empty() {
            break;
        }
        if let Some(after) = rest.strip_prefix('[') {
            let end = after.find(']')?;
            toks.push(parse_bracket_token(&after[..end])?);
            rest = &after[end + 1..];
        } else {
            // Object key up to the next '.' or '['.
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            toks.push(PathToken::Key(rest[..end].to_string()));
            rest = &rest[end..];
        }
    }
    Some(toks)
}

fn parse_bracket_token(inner: &str) -> Option<PathToken> {
    if inner == "*" {
        Some(PathToken::Wildcard)
    } else if let Some((a, b)) = inner.split_once(':') {
        let start = if a.is_empty() { None } else { Some(a.parse().ok()?) };
        let end = if b.is_empty() { None } else { Some(b.parse().ok()?) };
        Some(PathToken::Slice(start, end))
    } else {
        Some(PathToken::Index(inner.parse().ok()?))
    }
}

/// Walk `value` along `toks`. `Wildcard` projects the REMAINING tokens over each
/// element of the current array and collects the matches (dropping misses) — so
/// `$[0][*][0]` over `[[["a",..],["b",..]]]` yields `["a","b"]`.
fn walk_jsonpath(value: &serde_json::Value, toks: &[PathToken]) -> Option<serde_json::Value> {
    let Some((first, rest)) = toks.split_first() else {
        return Some(value.clone());
    };
    match first {
        PathToken::Key(k) => walk_jsonpath(value.get(k.as_str())?, rest),
        PathToken::Index(i) => {
            let arr = value.as_array()?;
            let idx: usize = if *i < 0 {
                (arr.len() as isize + *i).try_into().ok()?
            } else {
                *i as usize
            };
            walk_jsonpath(arr.get(idx)?, rest)
        }
        PathToken::Slice(start, end) => {
            let arr = value.as_array()?;
            let s = start.unwrap_or(0).min(arr.len());
            let e = end.unwrap_or(arr.len()).min(arr.len());
            let sub = if s <= e { arr[s..e].to_vec() } else { Vec::new() };
            walk_jsonpath(&serde_json::Value::Array(sub), rest)
        }
        PathToken::Wildcard => {
            let arr = value.as_array()?;
            let out: Vec<serde_json::Value> =
                arr.iter().filter_map(|el| walk_jsonpath(el, rest)).collect();
            Some(serde_json::Value::Array(out))
        }
    }
}

fn apply_jsonpath(value: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    let path = path.trim_start_matches("$.").trim_start_matches('$');
    if path.is_empty() {
        return Some(value.clone());
    }
    let toks = tokenize_jsonpath(path)?;
    walk_jsonpath(value, &toks)
}

/// Process conditional blocks: {{#if param}}...{{/if}} and {{#unless param}}...{{/unless}}.
// reviewed: offsets from find() + ASCII marker const .len() — char boundaries
#[allow(clippy::string_slice)]
fn process_conditionals(
    template: &str,
    params: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut result = template.to_string();

    // Process {{#if param}}...{{/if}}
    while let Some(start) = result.find("{{#if ") {
        let after_tag = start + 6; // length of "{{#if "
        let Some(close_tag) = result[after_tag..].find("}}") else {
            break;
        };
        let param_name = &result[after_tag..after_tag + close_tag];
        let block_start = after_tag + close_tag + 2;
        let end_tag = "{{/if}}";
        let Some(end_pos) = result[block_start..].find(end_tag) else {
            break;
        };
        let block_content = &result[block_start..block_start + end_pos];
        let full_end = block_start + end_pos + end_tag.len();

        let has_value = params.get(param_name).is_some_and(|v| !v.is_null());
        let replacement = if has_value {
            block_content.to_string()
        } else {
            String::new()
        };
        result = format!("{}{}{}", &result[..start], replacement, &result[full_end..]);
    }

    // Process {{#unless param}}...{{/unless}}
    while let Some(start) = result.find("{{#unless ") {
        let after_tag = start + 10; // length of "{{#unless "
        let Some(close_tag) = result[after_tag..].find("}}") else {
            break;
        };
        let param_name = &result[after_tag..after_tag + close_tag];
        let block_start = after_tag + close_tag + 2;
        let end_tag = "{{/unless}}";
        let Some(end_pos) = result[block_start..].find(end_tag) else {
            break;
        };
        let block_content = &result[block_start..block_start + end_pos];
        let full_end = block_start + end_pos + end_tag.len();

        let has_value = params.get(param_name).is_some_and(|v| !v.is_null());
        let replacement = if has_value {
            String::new()
        } else {
            block_content.to_string()
        };
        result = format!("{}{}{}", &result[..start], replacement, &result[full_end..]);
    }

    result
}

/// Render a body template: first resolve `${VAR}` secrets via `env_resolver`
/// (JSON-escaping substituted values for safe embedding in JSON bodies),
/// then substitute `{{param}}` placeholders with JSON-escaped parameter values.
///
/// Note on types: `params_map` is `serde_json::Map`, matching the call site's
/// `params.as_object().cloned()`. Consistent with `process_conditionals`.
///
/// Called from `execute()` on the `body_template` branch. Extracted as a pure
/// function for testability.
// reviewed: offsets from find("${")/find('}') + ASCII marker lengths — char boundaries
#[allow(clippy::string_slice)]
pub(crate) async fn render_body_template(
    template: &str,
    params_map: &serde_json::Map<String, serde_json::Value>,
    env_resolver: Option<&dyn EnvResolver>,
) -> String {
    // JSON-escape a raw string for safe embedding in JSON bodies.
    fn json_escape(raw: &str) -> String {
        raw.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    }

    // Phase 1: resolve ${VAR} secrets, JSON-escaping each substituted value.
    // We don't reuse `resolve_env_template` because that helper inserts raw
    // values (safe for HTTP headers, unsafe for JSON bodies where secrets
    // may contain " or \).
    let mut after_env = template.to_string();
    let mut start = 0;
    while let Some(open) = after_env[start..].find("${") {
        let abs_open = start + open;
        let Some(close_rel) = after_env[abs_open..].find('}') else {
            break;
        };
        let var_name = &after_env[abs_open + 2..abs_open + close_rel];
        let raw = resolve_env(var_name, env_resolver)
            .await
            .unwrap_or_default();
        let escaped = json_escape(&raw);
        after_env = format!(
            "{}{}{}",
            &after_env[..abs_open],
            &escaped,
            &after_env[abs_open + close_rel + 1..]
        );
        start = abs_open + escaped.len();
    }

    // Phase 2: conditionals then {{param}} substitution (existing behavior).
    let mut body = process_conditionals(&after_env, params_map);
    for (name, val) in params_map {
        let val_str = match val {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let escaped = json_escape(&val_str);
        body = body.replace(&format!("{{{{{name}}}}}"), &escaped);
    }
    body
}

/// Substitute ${`ENV_VAR`} in a template string, using `EnvResolver` if available.
// reviewed: offsets from find("${")/find('}') + ASCII marker lengths — char boundaries
#[allow(clippy::string_slice)]
async fn resolve_env_template(template: &str, env_resolver: Option<&dyn EnvResolver>) -> String {
    let mut result = template.to_string();
    // Find all ${VAR} patterns and replace
    let mut start = 0;
    while let Some(open) = result[start..].find("${") {
        let abs_open = start + open;
        if let Some(close) = result[abs_open..].find('}') {
            let var_name = &result[abs_open + 2..abs_open + close];
            let value = resolve_env(var_name, env_resolver)
                .await
                .unwrap_or_default();
            result = format!(
                "{}{}{}",
                &result[..abs_open],
                value,
                &result[abs_open + close + 1..]
            );
            start = abs_open + value.len();
        } else {
            break;
        }
    }
    result
}

// ── Loader ───────────────────────────────────────────────────────────────────

/// Load YAML tool definitions from `workspace/tools/*.yaml`.
///
/// Status is determined by the `status` field in each YAML file.
/// When `include_draft` is true, also includes draft tools.
/// Disabled tools are never loaded.
pub async fn load_yaml_tools(workspace_dir: &str, include_draft: bool) -> Vec<YamlToolDef> {
    let dir = Path::new(workspace_dir).join("tools");
    let mut tools = load_from_dir(&dir).await;

    tools.retain(|t| match t.status {
        ToolStatus::Verified => true,
        ToolStatus::Draft => include_draft,
        ToolStatus::Disabled => false,
    });

    tools
}

/// Deep merge two YAML values. `overlay` values override `base` values.
/// For Mapping types: keys from overlay override base, remaining base keys are kept.
/// Special handling for `extends` key: removed from result.
fn merge_yaml_values(base: serde_yaml::Value, overlay: serde_yaml::Value) -> serde_yaml::Value {
    use serde_yaml::Value;
    match (base, overlay) {
        (Value::Mapping(mut base_map), Value::Mapping(overlay_map)) => {
            for (key, overlay_val) in overlay_map {
                // Skip the extends key itself
                if matches!(&key, Value::String(s) if s == "extends") {
                    continue;
                }
                if let Some(base_val) = base_map.remove(&key) {
                    // Both have this key — deep merge for mappings, overlay wins otherwise
                    base_map.insert(key, merge_yaml_values(base_val, overlay_val));
                } else {
                    base_map.insert(key, overlay_val);
                }
            }
            Value::Mapping(base_map)
        }
        (_, overlay) => overlay, // Non-mapping: overlay wins
    }
}

/// Read all *.yaml files in a directory and parse them as `YamlToolDef`.
/// Supports template inheritance via `extends:` field — templates are loaded from `_templates/` subdirectory.
async fn load_from_dir(dir: &Path) -> Vec<YamlToolDef> {
    let mut tools = Vec::new();

    let mut read_dir = match fs::read_dir(dir).await {
        Ok(d) => d,
        Err(_) => return tools, // directory doesn't exist yet — ok
    };

    // Load templates from _templates/ subdirectory
    let templates_dir = dir.join("_templates");
    let mut templates: HashMap<String, serde_yaml::Value> = HashMap::new();
    if let Ok(mut tpl_dir) = fs::read_dir(&templates_dir).await {
        while let Ok(Some(entry)) = tpl_dir.next_entry().await {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if let Ok(content) = fs::read_to_string(&path).await
                && let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&content)
            {
                tracing::debug!(template = %name, "loaded YAML tool template");
                templates.insert(name, val);
            }
        }
    }

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }

        let content = match fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "failed to read YAML tool file");
                continue;
            }
        };

        // Try as Value first to check for extends
        let parsed = match serde_yaml::from_str::<serde_yaml::Value>(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let extends_name = parsed
            .get("extends")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        let tool_def = if let Some(extends_name) = &extends_name {
            // Merge with template
            if let Some(template) = templates.get(extends_name.as_str()) {
                let merged = merge_yaml_values(template.clone(), parsed);
                match serde_yaml::from_value::<YamlToolDef>(merged) {
                    Ok(def) => def,
                    Err(e) => {
                        tracing::warn!(
                            file = %path.display(),
                            template = %extends_name,
                            error = %e,
                            "failed to parse merged YAML tool"
                        );
                        continue;
                    }
                }
            } else {
                tracing::warn!(
                    file = %path.display(),
                    template = %extends_name,
                    "template not found"
                );
                continue;
            }
        } else {
            // No extends — parse directly
            match serde_yaml::from_value::<YamlToolDef>(parsed) {
                Ok(def) => def,
                Err(_) => {
                    // Silently skip non-tool YAML files (e.g. service configs)
                    continue;
                }
            }
        };

        tracing::debug!(tool = %tool_def.name, status = ?tool_def.status, "loaded YAML tool");
        tools.push(tool_def);
    }

    tools
}

/// Load all YAML tool definitions (all statuses) from `workspace/tools/`.
/// Used by the management API to show all tools with their current status.
pub async fn load_all_yaml_tools(workspace_dir: &str) -> Vec<YamlToolDef> {
    let dir = Path::new(workspace_dir).join("tools");
    load_from_dir(&dir).await
}

/// Find a YAML tool by name, searching root, verified, and draft directories.
pub async fn find_yaml_tool(workspace_dir: &str, tool_name: &str) -> Option<YamlToolDef> {
    let tools = load_yaml_tools(workspace_dir, true).await;
    tools.into_iter().find(|t| t.name == tool_name)
}

/// Return the workspace path for a tool YAML file.
/// All tools live flat in `workspace/tools/`.
pub fn tool_file_path(workspace_dir: &str, _status: &ToolStatus, name: &str) -> std::path::PathBuf {
    Path::new(workspace_dir)
        .join("tools")
        .join(format!("{name}.yaml"))
}

// ── OpenAPI security scheme → YamlAuth translation ──────────────────────────

/// Convert an `OpenAPI` security scheme JSON to a `YamlAuth` config.
/// Supports apiKey (header/query), http (bearer/basic), and oauth2 schemes.
#[cfg(test)]
pub fn openapi_security_to_yaml_auth(scheme: &serde_json::Value) -> Option<YamlAuth> {
    let scheme_type = scheme.get("type")?.as_str()?;
    match scheme_type {
        "apiKey" => {
            let location = scheme.get("in")?.as_str()?;
            let name = scheme
                .get("name")
                .and_then(|n| n.as_str())
                .map(String::from);
            match location {
                "header" => Some(YamlAuth {
                    auth_type: "api_key_header".into(),
                    header_name: name,
                    key: None,
                    username_key: None,
                    password_key: None,
                    param_name: None,
                    headers: None,
                    token_url: None,
                    token_body: None,
                    token_field: None,
                }),
                "query" => Some(YamlAuth {
                    auth_type: "api_key_query".into(),
                    param_name: name,
                    key: None,
                    username_key: None,
                    password_key: None,
                    header_name: None,
                    headers: None,
                    token_url: None,
                    token_body: None,
                    token_field: None,
                }),
                _ => None,
            }
        }
        "http" => {
            let http_scheme = scheme.get("scheme")?.as_str()?;
            match http_scheme {
                "bearer" => Some(YamlAuth {
                    auth_type: "bearer_env".into(),
                    key: None,
                    username_key: None,
                    password_key: None,
                    header_name: None,
                    param_name: None,
                    headers: None,
                    token_url: None,
                    token_body: None,
                    token_field: None,
                }),
                "basic" => Some(YamlAuth {
                    auth_type: "basic_env".into(),
                    key: None,
                    username_key: None,
                    password_key: None,
                    header_name: None,
                    param_name: None,
                    headers: None,
                    token_url: None,
                    token_body: None,
                    token_field: None,
                }),
                _ => None,
            }
        }
        "oauth2" => Some(YamlAuth {
            auth_type: "oauth_refresh".into(),
            key: None,
            username_key: None,
            password_key: None,
            header_name: None,
            param_name: None,
            headers: None,
            token_url: None,
            token_body: None,
            token_field: None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::ERROR_BODY_MAX_CHARS;

    // ── Test helpers ─────────────────────────────────────────────────────────

    /// Test-only `EnvResolver` backed by an in-memory `HashMap`.
    /// Named-field form so tests can construct it with
    /// `MapResolver { map: secrets }`.
    struct MapResolver {
        map: HashMap<String, String>,
    }

    #[async_trait]
    impl EnvResolver for MapResolver {
        async fn resolve(&self, key: &str) -> Option<String> {
            self.map.get(key).cloned()
        }
    }

    // ── redact_secrets ───────────────────────────────────────────────────────

    #[test]
    fn redact_secrets_bearer_token_is_redacted() {
        // Bug 7/8/10: Bearer tokens in error bodies must be redacted.
        let input = r#"{"error":"invalid request","Authorization":"Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9"}"#;
        let out = crate::redact::redact_secrets(input);
        assert!(
            !out.contains("eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9"),
            "raw JWT must not appear: {out}"
        );
        assert!(out.contains("[REDACTED]"), "must contain [REDACTED]: {out}");
    }

    #[test]
    fn redact_secrets_plain_text_untouched() {
        // Strings with no secret patterns must pass through unchanged (modulo truncation).
        let input = "error: resource not found, id=12345";
        let out = crate::redact::redact_secrets(input);
        assert_eq!(out, input, "plain error text must not be modified");
    }

    #[test]
    fn redact_secrets_truncates_long_body() {
        // Bodies longer than ERROR_BODY_MAX_CHARS must be truncated.
        let long = "x".repeat(ERROR_BODY_MAX_CHARS + 100);
        let out = crate::redact::redact_secrets(&long);
        assert_eq!(
            out.len(),
            ERROR_BODY_MAX_CHARS,
            "output must be truncated to {ERROR_BODY_MAX_CHARS} chars"
        );
    }

    #[test]
    fn redact_secrets_short_body_not_truncated() {
        let input = "short error";
        let out = crate::redact::redact_secrets(input);
        assert_eq!(out, input, "short body must not be truncated or modified");
    }

    #[test]
    fn redact_secrets_api_key_pattern_redacted() {
        let input = "invalid api_key abcdef123456 provided";
        let out = crate::redact::redact_secrets(input);
        assert!(
            !out.contains("abcdef123456"),
            "api_key value must be redacted: {out}"
        );
        assert!(out.contains("[REDACTED]"), "must contain [REDACTED]: {out}");
    }

    // ── apply_jsonpath ───────────────────────────────────────────────────────

    #[test]
    fn apply_jsonpath_root_dollar() {
        let val = serde_json::json!({"a": 1});
        assert_eq!(apply_jsonpath(&val, "$"), Some(val.clone()));
    }

    #[test]
    fn apply_jsonpath_empty_path() {
        let val = serde_json::json!({"a": 1});
        assert_eq!(apply_jsonpath(&val, ""), Some(val.clone()));
    }

    #[test]
    fn apply_jsonpath_top_level_key() {
        let val = serde_json::json!({"key": "hello"});
        assert_eq!(
            apply_jsonpath(&val, "$.key"),
            Some(serde_json::json!("hello"))
        );
    }

    #[test]
    fn apply_jsonpath_nested_key() {
        let val = serde_json::json!({"key": {"nested": 42}});
        assert_eq!(
            apply_jsonpath(&val, "$.key.nested"),
            Some(serde_json::json!(42))
        );
    }

    #[test]
    fn apply_jsonpath_array_element() {
        let val = serde_json::json!({"items": [10, 20, 30]});
        assert_eq!(
            apply_jsonpath(&val, "$.items[0]"),
            Some(serde_json::json!(10))
        );
    }

    #[test]
    fn apply_jsonpath_missing_key() {
        let val = serde_json::json!({"key": 1});
        assert_eq!(apply_jsonpath(&val, "$.missing"), None);
    }

    #[test]
    fn apply_jsonpath_multi_level_nested() {
        let val = serde_json::json!({"key": {"deep": {"nested": true}}});
        assert_eq!(
            apply_jsonpath(&val, "$.key.deep.nested"),
            Some(serde_json::json!(true))
        );
    }

    #[test]
    fn apply_jsonpath_consecutive_brackets() {
        // Google-Translate-style nested arrays: previously `[0][0][0]` failed to
        // parse (one bracket per dot-segment) and the transform silently no-op'd.
        let val = serde_json::json!([[["Hello. ", "Привет. "]], null, "ru"]);
        assert_eq!(
            apply_jsonpath(&val, "$[0][0][0]"),
            Some(serde_json::json!("Hello. "))
        );
    }

    #[test]
    fn apply_jsonpath_wildcard_projects_remaining_path() {
        // `$[0][*][0]` projects [0] over every segment → array of translations.
        let val = serde_json::json!([
            [["Hello. ", "Привет. "], ["How are you?", "Как дела?"]],
            null, "ru"
        ]);
        assert_eq!(
            apply_jsonpath(&val, "$[0][*][0]"),
            Some(serde_json::json!(["Hello. ", "How are you?"]))
        );
    }

    #[test]
    fn translate_pipeline_joins_all_segments() {
        // End-to-end: the real Google response shape → transform → join → one string.
        let resp = serde_json::json!([
            [["Hello. ", "Привет. "], ["How are you today?", "Как дела сегодня?"]],
            null, "ru"
        ]);
        let extracted = apply_jsonpath(&resp, "$[0][*][0]").unwrap();
        let joined = apply_pipeline(extracted, &[ResponsePipelineStep::Join(String::new())]);
        assert_eq!(joined, serde_json::json!("Hello. How are you today?"));
    }

    #[test]
    fn yaml_auth_accepts_type_and_auth_type_alias() {
        // Canonical `type:` and the `auth_type:` alias both populate auth_type,
        // so a tool authored with `auth_type:` isn't silently parsed as no-auth.
        let canonical: YamlAuth = serde_yaml::from_str("type: bearer_env\nkey: X").unwrap();
        assert_eq!(canonical.auth_type, "bearer_env");
        let aliased: YamlAuth = serde_yaml::from_str("auth_type: bearer_env\nkey: X").unwrap();
        assert_eq!(aliased.auth_type, "bearer_env");
    }

    // ── resolve_env_template ─────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_env_template_no_pattern() {
        assert_eq!(
            resolve_env_template("plain string", None).await,
            "plain string"
        );
    }

    #[tokio::test]
    async fn resolve_env_template_nonexistent_var() {
        // Use a var name that is extremely unlikely to exist
        let result =
            resolve_env_template("prefix-${__OPEX_TEST_NONEXISTENT_XYZ__}-suffix", None).await;
        assert_eq!(result, "prefix--suffix");
    }

    #[tokio::test]
    async fn resolve_env_template_with_resolver() {
        use std::collections::HashMap;
        struct MapResolver(HashMap<String, String>);
        #[async_trait]
        impl EnvResolver for MapResolver {
            async fn resolve(&self, key: &str) -> Option<String> {
                self.0.get(key).cloned()
            }
        }
        let resolver = MapResolver(HashMap::from([(
            "__OPEX_YAML_TOOLS_TEST_VAR__".into(),
            "resolved_value".into(),
        )]));
        let result = resolve_env_template(
            "Bearer ${__OPEX_YAML_TOOLS_TEST_VAR__}",
            Some(&resolver),
        )
        .await;
        assert_eq!(result, "Bearer resolved_value");
    }

    // ── YamlToolDef::to_tool_definition ──────────────────────────────────────

    fn make_test_tool() -> YamlToolDef {
        let mut params = HashMap::new();
        params.insert(
            "query".to_string(),
            YamlParam {
                param_type: "string".to_string(),
                required: true,
                location: ParamLocation::Query,
                description: "Search query".to_string(),
                default: None,
                enum_values: vec![],
                minimum: None,
                maximum: None,
                examples: vec![],
                default_from_env: None,
            },
        );
        params.insert(
            "format".to_string(),
            YamlParam {
                param_type: "string".to_string(),
                required: false,
                location: ParamLocation::Query,
                description: "Output format".to_string(),
                default: Some(serde_json::json!("json")),
                enum_values: vec!["json".into(), "xml".into(), "csv".into()],
                minimum: None,
                maximum: None,
                examples: vec![],
                default_from_env: None,
            },
        );
        params.insert(
            "count".to_string(),
            YamlParam {
                param_type: "integer".to_string(),
                required: false,
                location: ParamLocation::Query,
                description: "Number of results".to_string(),
                default: None,
                enum_values: vec![],
                minimum: Some(1.0),
                maximum: Some(100.0),
                examples: vec![],
                default_from_env: None,
            },
        );

        YamlToolDef {
            extends: None,
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            tags: vec![],
            endpoint: "https://example.com/api".to_string(),
            method: "GET".to_string(),
            headers: HashMap::new(),
            parameters: params,
            auth: None,
            body_template: None,
            response_transform: None,
            channel_action: None,
            status: ToolStatus::Verified,
            created_by: String::new(),
            timeout: 60,
            retry: None,
            content_type: "application/json".to_string(),
            cache: None,
            pagination: None,
            response_schema: None,
            graphql: None,
            response_pipeline: vec![],
            required_base: false,
            allow_private_endpoint: false,
            parallel: false,
            required_secrets: vec![],
        }
    }

    #[test]
    fn to_tool_definition_name_and_description() {
        let tool = make_test_tool();
        let td = tool.to_tool_definition();
        assert_eq!(td.name, "test_tool");
        assert_eq!(td.description, "A test tool");
    }

    #[test]
    fn to_tool_definition_required_params() {
        let tool = make_test_tool();
        let td = tool.to_tool_definition();
        let required = td.input_schema["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"query"), "query must be required");
        assert!(
            !required_names.contains(&"format"),
            "format must not be required"
        );
        assert!(
            !required_names.contains(&"count"),
            "count must not be required"
        );
    }

    #[test]
    fn to_tool_definition_enum_values() {
        let tool = make_test_tool();
        let td = tool.to_tool_definition();
        let format_prop = &td.input_schema["properties"]["format"];
        let enum_vals = format_prop["enum"].as_array().unwrap();
        let enum_strs: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(enum_strs, vec!["json", "xml", "csv"]);
    }

    #[test]
    fn to_tool_definition_min_max() {
        let tool = make_test_tool();
        let td = tool.to_tool_definition();
        let count_prop = &td.input_schema["properties"]["count"];
        assert_eq!(count_prop["minimum"].as_f64(), Some(1.0));
        assert_eq!(count_prop["maximum"].as_f64(), Some(100.0));
    }

    // ── ToolStatus serde roundtrip ───────────────────────────────────────────

    #[test]
    fn tool_status_serde_roundtrip() {
        for (status, expected_str) in [
            (ToolStatus::Verified, "\"verified\""),
            (ToolStatus::Draft, "\"draft\""),
            (ToolStatus::Disabled, "\"disabled\""),
        ] {
            let serialized = serde_json::to_string(&status).unwrap();
            assert_eq!(serialized, expected_str);
            let deserialized: ToolStatus = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, status);
        }
    }

    // ── ParamLocation serde (deserialize only — no Serialize derive) ─────────

    #[test]
    fn param_location_deserialize() {
        for (json_str, expected) in [
            ("\"path\"", ParamLocation::Path),
            ("\"query\"", ParamLocation::Query),
            ("\"body\"", ParamLocation::Body),
            ("\"header\"", ParamLocation::Header),
        ] {
            let deserialized: ParamLocation = serde_json::from_str(json_str).unwrap();
            assert_eq!(deserialized, expected);
        }
    }

    // ── tool_file_path ──

    #[test]
    fn tool_file_path_builds_correct_path() {
        let path = tool_file_path("/workspace", &ToolStatus::Verified, "my_tool");
        assert_eq!(
            path,
            std::path::PathBuf::from("/workspace/tools/my_tool.yaml")
        );
    }

    #[test]
    fn tool_file_path_ignores_status() {
        let a = tool_file_path("/ws", &ToolStatus::Draft, "t");
        let b = tool_file_path("/ws", &ToolStatus::Verified, "t");
        let c = tool_file_path("/ws", &ToolStatus::Disabled, "t");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    // ── Phase 1: retry config ───────────────────────────────────────────────

    #[test]
    fn retry_config_deserialize_full() {
        let yaml = "max_attempts: 3\nbackoff_base_ms: 2000\nretry_on: [429, 503]";
        let cfg: YamlRetryConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.max_attempts, 3);
        assert_eq!(cfg.backoff_base_ms, 2000);
        assert_eq!(cfg.retry_on, vec![429, 503]);
    }

    #[test]
    fn retry_config_defaults() {
        let cfg: YamlRetryConfig = serde_yaml::from_str("max_attempts: 2").unwrap();
        assert_eq!(cfg.max_attempts, 2);
        assert_eq!(cfg.backoff_base_ms, 1000);
        assert_eq!(cfg.retry_on, vec![429, 500, 502, 503, 504]);
    }

    #[test]
    fn tool_with_timeout_and_retry_deserializes() {
        let yaml = r#"
name: test
description: test tool
endpoint: https://example.com
method: GET
timeout: 15
retry:
  max_attempts: 3
content_type: application/x-www-form-urlencoded
"#;
        let tool: YamlToolDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(tool.timeout, 15);
        assert!(tool.retry.is_some());
        assert_eq!(tool.retry.unwrap().max_attempts, 3);
        assert_eq!(tool.content_type, "application/x-www-form-urlencoded");
    }

    #[test]
    fn tool_timeout_defaults_to_60() {
        let yaml = r#"
name: test
description: test tool
endpoint: https://example.com
method: GET
"#;
        let tool: YamlToolDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(tool.timeout, 60);
        assert!(tool.retry.is_none());
        assert_eq!(tool.content_type, "application/json");
    }

    // ── Phase 1: resolve_env_template with resolver ─────────────────────────

    #[tokio::test]
    async fn resolve_env_template_uses_resolver() {
        struct TestResolver;
        #[async_trait]
        impl EnvResolver for TestResolver {
            async fn resolve(&self, key: &str) -> Option<String> {
                if key == "MY_SCOPED_KEY" {
                    Some("scoped_value".into())
                } else {
                    None
                }
            }
        }
        let result = resolve_env_template("Bearer ${MY_SCOPED_KEY}", Some(&TestResolver)).await;
        assert_eq!(result, "Bearer scoped_value");
    }

    #[tokio::test]
    async fn resolve_env_template_resolver_fallback_to_env() {
        // Use PATH which exists on all platforms (Windows, Linux, macOS)
        struct EmptyResolver;
        #[async_trait]
        impl EnvResolver for EmptyResolver {
            async fn resolve(&self, _: &str) -> Option<String> {
                None
            }
        }
        // EmptyResolver returns None -> resolve_env falls back to std::env::var
        let result = resolve_env_template("x${PATH}y", Some(&EmptyResolver)).await;
        // PATH always exists and is non-empty, so result should be "x<PATH_VALUE>y"
        assert!(
            result.starts_with("x"),
            "result should start with 'x': {result}"
        );
        assert!(
            result.ends_with("y"),
            "result should end with 'y': {result}"
        );
        assert!(result.len() > 2, "PATH should be non-empty");
        assert!(!result.contains("${PATH}"), "variable should be resolved");
    }

    // ── Phase 2: cache key ──────────────────────────────────────────────────

    #[test]
    fn cache_key_uses_specified_params() {
        let key1 = build_cache_key(
            "agent",
            "tool",
            "GET",
            "https://example.com",
            &serde_json::json!({"ticker": "AAPL", "extra": 1}),
            &["ticker".into()],
        );
        let key2 = build_cache_key(
            "agent",
            "tool",
            "GET",
            "https://example.com",
            &serde_json::json!({"ticker": "AAPL", "extra": 2}),
            &["ticker".into()],
        );
        assert_eq!(key1, key2); // extra ignored
    }

    #[test]
    fn cache_key_all_params_when_empty() {
        let key1 = build_cache_key(
            "agent",
            "t",
            "GET",
            "https://example.com",
            &serde_json::json!({"a": 1, "b": 2}),
            &[],
        );
        let key2 = build_cache_key(
            "agent",
            "t",
            "GET",
            "https://example.com",
            &serde_json::json!({"a": 1, "b": 3}),
            &[],
        );
        assert_ne!(key1, key2);
    }

    #[test]
    fn cache_key_object_keys_are_order_independent() {
        let a = build_cache_key(
            "agent",
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"a": 1, "b": 2}),
            &[],
        );
        let b = build_cache_key(
            "agent",
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"b": 2, "a": 1}),
            &[],
        );
        assert_eq!(
            a, b,
            "object key order must not matter (serde_json::Map is BTreeMap)"
        );
    }

    #[test]
    fn cache_key_array_order_matters() {
        let a = build_cache_key(
            "agent",
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"tags": ["a", "b"]}),
            &[],
        );
        let b = build_cache_key(
            "agent",
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"tags": ["b", "a"]}),
            &[],
        );
        assert_ne!(a, b, "array element order is part of the cache key");
    }

    #[test]
    fn f039_cache_key_is_scoped_per_agent() {
        // Same tool + params but different agents must produce DIFFERENT keys,
        // else agent B reads agent A's authenticated private response.
        let mk = |agent: &str| {
            build_cache_key(
                agent,
                "my_notifications",
                "GET",
                "https://api.example.com/notifications",
                &serde_json::json!({}),
                &[],
            )
        };
        assert_ne!(mk("agentA"), mk("agentB"), "cache key must be agent-scoped");
        assert_eq!(mk("agentA"), mk("agentA"), "stable for the same agent");
    }

    #[tokio::test]
    async fn cache_evicts_oldest_at_cap_with_min_one() {
        // max_entries = 3 -> eviction target = max(3/10, 1) = 1 per write.
        let ctx = ToolExecutionContext::new(3);
        ctx.set_cached("k1", "v1", 60).await;
        ctx.set_cached("k2", "v2", 60).await;
        ctx.set_cached("k3", "v3", 60).await;
        assert_eq!(ctx.cache_len(), 3);
        ctx.set_cached("k4", "v4", 60).await;
        assert!(ctx.cache_len() <= 3, "soft cap must hold at max_entries");
        assert!(
            ctx.get_cached("k4").await.is_some(),
            "newest write must be present"
        );
    }

    #[test]
    fn cache_config_deserialize() {
        let yaml = "ttl: 300\nkey_params: [ticker, period]";
        let cfg: YamlCacheConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.ttl, 300);
        assert_eq!(cfg.key_params, vec!["ticker", "period"]);
    }

    #[test]
    fn pagination_config_deserialize() {
        let yaml = "type: offset\nparam: offset\nlimit: 50\nmax_pages: 3\nresults_path: $.data";
        let cfg: YamlPaginationConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.pagination_type, "offset");
        assert_eq!(cfg.limit, Some(50));
        assert_eq!(cfg.max_pages, Some(3));
        assert_eq!(cfg.results_path.as_deref(), Some("$.data"));
    }

    #[test]
    fn pagination_cursor_config() {
        let yaml = "type: cursor\nparam: cursor\nnext_path: $.meta.next_cursor";
        let cfg: YamlPaginationConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.pagination_type, "cursor");
        assert_eq!(cfg.next_path.as_deref(), Some("$.meta.next_cursor"));
    }

    #[tokio::test]
    async fn execution_context_cache_basic() {
        let ctx = ToolExecutionContext::new(1000);
        assert!(ctx.get_cached("key").await.is_none());
        ctx.set_cached("key", "value", 60).await;
        assert_eq!(ctx.get_cached("key").await, Some("value".to_string()));
    }

    #[test]
    fn tool_with_cache_and_pagination_deserializes() {
        let yaml = r#"
name: search
description: paginated search
endpoint: https://example.com/search
method: GET
cache:
  ttl: 120
  key_params: [query]
pagination:
  type: offset
  param: offset
  limit_param: limit
  limit: 20
  max_pages: 3
  results_path: "$.results"
"#;
        let tool: YamlToolDef = serde_yaml::from_str(yaml).unwrap();
        assert!(tool.cache.is_some());
        assert_eq!(tool.cache.as_ref().unwrap().ttl, 120);
        assert!(tool.pagination.is_some());
        assert_eq!(tool.pagination.as_ref().unwrap().pagination_type, "offset");
    }

    // ── Phase 3: enhanced JSONPath ──────────────────────────────────────────

    #[test]
    fn jsonpath_wildcard() {
        let val = serde_json::json!({"items": [{"id": 1}, {"id": 2}]});
        let result = apply_jsonpath(&val, "$.items[*]");
        assert_eq!(result, Some(serde_json::json!([{"id": 1}, {"id": 2}])));
    }

    #[test]
    fn jsonpath_negative_index() {
        let val = serde_json::json!({"items": [10, 20, 30]});
        assert_eq!(
            apply_jsonpath(&val, "$.items[-1]"),
            Some(serde_json::json!(30))
        );
    }

    #[test]
    fn jsonpath_negative_index_first() {
        let val = serde_json::json!({"items": [10, 20, 30]});
        assert_eq!(
            apply_jsonpath(&val, "$.items[-3]"),
            Some(serde_json::json!(10))
        );
    }

    #[test]
    fn jsonpath_slice() {
        let val = serde_json::json!({"items": [10, 20, 30, 40]});
        assert_eq!(
            apply_jsonpath(&val, "$.items[0:2]"),
            Some(serde_json::json!([10, 20]))
        );
    }

    #[test]
    fn jsonpath_slice_open_end() {
        let val = serde_json::json!({"items": [10, 20, 30]});
        assert_eq!(
            apply_jsonpath(&val, "$.items[1:]"),
            Some(serde_json::json!([20, 30]))
        );
    }

    // ── Phase 3: conditional templates ───────────────────────────────────────

    #[test]
    fn conditional_if_present() {
        let params = serde_json::json!({"ticker": "AAPL", "period": "1d"})
            .as_object()
            .unwrap()
            .clone();
        let result = process_conditionals(
            r#"{"ticker":"{{ticker}}"{{#if period}},"period":"{{period}}"{{/if}}}"#,
            &params,
        );
        assert!(result.contains("period"));
    }

    #[test]
    fn conditional_if_absent() {
        let params = serde_json::json!({"ticker": "AAPL"})
            .as_object()
            .unwrap()
            .clone();
        let result = process_conditionals(
            r#"{"ticker":"{{ticker}}"{{#if period}},"period":"{{period}}"{{/if}}}"#,
            &params,
        );
        assert!(!result.contains("period"));
    }

    #[test]
    fn conditional_unless_present() {
        let params = serde_json::json!({"limit": 10})
            .as_object()
            .unwrap()
            .clone();
        let result =
            process_conditionals("base{{#unless limit}},default_limit{{/unless}}", &params);
        assert_eq!(result, "base");
    }

    #[test]
    fn conditional_unless_absent() {
        let params = serde_json::Map::new();
        let result =
            process_conditionals("base{{#unless limit}},default_limit{{/unless}}", &params);
        assert_eq!(result, "base,default_limit");
    }

    // ── Phase 3: response_schema ────────────────────────────────────────────

    #[test]
    fn response_schema_appended_to_description() {
        let mut tool = make_test_tool();
        tool.response_schema =
            Some(serde_json::json!({"type": "object", "fields": {"price": "current price"}}));
        let td = tool.to_tool_definition();
        assert!(td.description.contains("Response schema:"));
        assert!(td.description.contains("price"));
    }

    #[test]
    fn response_schema_none_keeps_description() {
        let tool = make_test_tool();
        let td = tool.to_tool_definition();
        assert_eq!(td.description, "A test tool");
    }

    // ── Phase 4: GraphQL config ─────────────────────────────────────────────

    #[test]
    fn graphql_config_deserialize() {
        let yaml = r#"
query: "query($t: String!) { stock(ticker: $t) { price } }"
variables:
  t: "{{ticker}}"
"#;
        let cfg: YamlGraphqlConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.query.contains("stock"));
        assert!(cfg.variables.is_some());
        assert_eq!(
            cfg.variables.as_ref().unwrap().get("t").unwrap(),
            "{{ticker}}"
        );
    }

    #[test]
    fn graphql_config_without_variables() {
        let yaml = r#"query: "{ viewer { login } }""#;
        let cfg: YamlGraphqlConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.variables.is_none());
    }

    #[test]
    fn tool_with_graphql_deserializes() {
        let yaml = r#"
name: gql_tool
description: GraphQL test
endpoint: https://api.example.com/graphql
method: POST
graphql:
  query: "query($t: String!) { stock(ticker: $t) { price } }"
  variables:
    t: "{{ticker}}"
"#;
        let tool: YamlToolDef = serde_yaml::from_str(yaml).unwrap();
        assert!(tool.graphql.is_some());
        assert!(tool.graphql.as_ref().unwrap().query.contains("stock"));
    }

    // ── Phase 4: response pipeline ──────────────────────────────────────────

    #[test]
    fn pipeline_pick_fields_and_limit() {
        let data = serde_json::json!([
            {"ticker": "AAPL", "price": 150, "extra": true},
            {"ticker": "GOOG", "price": 2800, "extra": false},
            {"ticker": "MSFT", "price": 300, "extra": true},
        ]);
        let pipeline = vec![
            ResponsePipelineStep::PickFields(vec!["ticker".into(), "price".into()]),
            ResponsePipelineStep::Limit(2),
        ];
        let result = apply_pipeline(data, &pipeline);
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0].get("extra").is_none());
        assert!(arr[0].get("ticker").is_some());
    }

    #[test]
    fn pipeline_sort_by_asc() {
        let data = serde_json::json!([
            {"name": "B", "val": 30},
            {"name": "A", "val": 10},
            {"name": "C", "val": 20},
        ]);
        let pipeline = vec![ResponsePipelineStep::SortBy {
            field: "val".into(),
            desc: false,
        }];
        let result = apply_pipeline(data, &pipeline);
        let arr = result.as_array().unwrap();
        assert_eq!(arr[0]["name"], "A");
        assert_eq!(arr[1]["name"], "C");
        assert_eq!(arr[2]["name"], "B");
    }

    #[test]
    fn pipeline_sort_by_desc() {
        let data = serde_json::json!([
            {"name": "B", "val": 30},
            {"name": "A", "val": 10},
        ]);
        let pipeline = vec![ResponsePipelineStep::SortBy {
            field: "val".into(),
            desc: true,
        }];
        let result = apply_pipeline(data, &pipeline);
        let arr = result.as_array().unwrap();
        assert_eq!(arr[0]["name"], "B");
        assert_eq!(arr[1]["name"], "A");
    }

    #[test]
    fn pipeline_jsonpath_then_limit() {
        let data = serde_json::json!({"results": [1, 2, 3, 4, 5]});
        let pipeline = vec![
            ResponsePipelineStep::Jsonpath("$.results".into()),
            ResponsePipelineStep::Limit(3),
        ];
        let result = apply_pipeline(data, &pipeline);
        assert_eq!(result, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn pipeline_empty_is_identity() {
        let data = serde_json::json!({"foo": "bar"});
        let result = apply_pipeline(data.clone(), &[]);
        assert_eq!(result, data);
    }

    #[test]
    fn response_pipeline_deserialize() {
        let yaml = r#"
- jsonpath: "$.data"
- pick_fields: ["name", "price"]
- sort_by:
    field: price
    desc: true
- limit: 5
"#;
        let pipeline: Vec<ResponsePipelineStep> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pipeline.len(), 4);
    }

    // ── Phase 4: OpenAPI auth translation ───────────────────────────────────

    #[test]
    fn openapi_bearer_scheme() {
        let scheme = serde_json::json!({"type": "http", "scheme": "bearer"});
        let auth = openapi_security_to_yaml_auth(&scheme).unwrap();
        assert_eq!(auth.auth_type, "bearer_env");
    }

    #[test]
    fn openapi_basic_scheme() {
        let scheme = serde_json::json!({"type": "http", "scheme": "basic"});
        let auth = openapi_security_to_yaml_auth(&scheme).unwrap();
        assert_eq!(auth.auth_type, "basic_env");
    }

    #[test]
    fn openapi_api_key_header() {
        let scheme = serde_json::json!({"type": "apiKey", "in": "header", "name": "X-API-Key"});
        let auth = openapi_security_to_yaml_auth(&scheme).unwrap();
        assert_eq!(auth.auth_type, "api_key_header");
        assert_eq!(auth.header_name.as_deref(), Some("X-API-Key"));
    }

    #[test]
    fn openapi_api_key_query() {
        let scheme = serde_json::json!({"type": "apiKey", "in": "query", "name": "api_key"});
        let auth = openapi_security_to_yaml_auth(&scheme).unwrap();
        assert_eq!(auth.auth_type, "api_key_query");
        assert_eq!(auth.param_name.as_deref(), Some("api_key"));
    }

    #[test]
    fn openapi_oauth2() {
        let scheme = serde_json::json!({"type": "oauth2", "flows": {"authorizationCode": {}}});
        let auth = openapi_security_to_yaml_auth(&scheme).unwrap();
        assert_eq!(auth.auth_type, "oauth_refresh");
    }

    #[test]
    fn openapi_unknown_scheme_returns_none() {
        let scheme = serde_json::json!({"type": "openIdConnect"});
        assert!(openapi_security_to_yaml_auth(&scheme).is_none());
    }

    // ── resolve_env_template ──────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_env_template_handles_multiple_vars() {
        use std::collections::HashMap;
        let mut secrets = HashMap::new();
        secrets.insert("SMTP_HOST".to_string(), "smtp.example.com".to_string());
        secrets.insert("SMTP_PORT".to_string(), "587".to_string());
        secrets.insert("EMAIL_USER".to_string(), "user@example.com".to_string());
        secrets.insert("EMAIL_PASS".to_string(), "s3cret".to_string());
        let resolver = MapResolver { map: secrets };

        let template = r#"{"server":"${SMTP_HOST}","port":${SMTP_PORT},"user":"${EMAIL_USER}","password":"${EMAIL_PASS}","to":"{{to}}"}"#;
        let after_env = resolve_env_template(template, Some(&resolver)).await;

        // Every ${VAR} was substituted; {{to}} is left for the next phase.
        assert!(after_env.contains("smtp.example.com"));
        assert!(after_env.contains(r#""port":587"#));
        assert!(after_env.contains("user@example.com"));
        assert!(after_env.contains("s3cret"));
        assert!(after_env.contains(r#""to":"{{to}}""#));
    }

    // ── render_body_template ─────────────────────────────────────────────────

    #[tokio::test]
    async fn render_body_template_resolves_secret_before_params() {
        use std::collections::HashMap;
        let mut secrets = HashMap::new();
        secrets.insert("SMTP_HOST".to_string(), "smtp.test.com".to_string());
        let resolver = MapResolver { map: secrets };

        let mut params = serde_json::Map::new();
        params.insert(
            "to".to_string(),
            serde_json::Value::String("x@y.com".to_string()),
        );

        let template = r#"{"server":"${SMTP_HOST}","to":"{{to}}"}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;

        let parsed: serde_json::Value = serde_json::from_str(&rendered)
            .unwrap_or_else(|e| panic!("render did not produce valid JSON: {e} — got: {rendered}"));
        assert_eq!(parsed["server"], "smtp.test.com");
        assert_eq!(parsed["to"], "x@y.com");
    }

    #[tokio::test]
    async fn render_body_template_missing_secret_is_empty() {
        use std::collections::HashMap;
        let resolver = MapResolver {
            map: HashMap::new(),
        };
        let params = serde_json::Map::new();
        let template = r#"{"host":"${MISSING}"}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;
        assert_eq!(rendered, r#"{"host":""}"#);
    }

    #[tokio::test]
    async fn render_body_template_param_with_quotes_is_escaped() {
        use std::collections::HashMap;
        let resolver = MapResolver {
            map: HashMap::new(),
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "body".to_string(),
            serde_json::Value::String(r#"hello "world""#.to_string()),
        );
        let template = r#"{"body":"{{body}}"}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["body"], r#"hello "world""#);
    }

    #[tokio::test]
    async fn render_body_template_secret_with_quotes_is_escaped() {
        // C2 regression test: secrets with JSON special chars must not break the body.
        use std::collections::HashMap;
        let mut secrets = HashMap::new();
        secrets.insert("PASS".to_string(), r#"p@ss"with\backslash"#.to_string());
        let resolver = MapResolver { map: secrets };

        let params = serde_json::Map::new();
        let template = r#"{"password":"${PASS}"}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;

        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap_or_else(|e| {
            panic!("render produced invalid JSON with escaped secret: {e} — got: {rendered}")
        });
        assert_eq!(parsed["password"], r#"p@ss"with\backslash"#);
    }

    #[tokio::test]
    async fn render_body_template_multiple_secrets_all_resolved() {
        // I2 regression test: multiple ${VAR} in one template.
        use std::collections::HashMap;
        let mut secrets = HashMap::new();
        secrets.insert("SMTP_HOST".to_string(), "smtp.example.com".to_string());
        secrets.insert("EMAIL_USER".to_string(), "u@example.com".to_string());
        secrets.insert("EMAIL_PASS".to_string(), "pw".to_string());
        secrets.insert("IMAP_HOST".to_string(), "imap.example.com".to_string());
        let resolver = MapResolver { map: secrets };

        let mut params = serde_json::Map::new();
        params.insert(
            "to".to_string(),
            serde_json::Value::String("x@y.com".to_string()),
        );

        let template = r#"{"smtp":"${SMTP_HOST}","imap":"${IMAP_HOST}","user":"${EMAIL_USER}","pass":"${EMAIL_PASS}","to":"{{to}}"}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["smtp"], "smtp.example.com");
        assert_eq!(parsed["imap"], "imap.example.com");
        assert_eq!(parsed["user"], "u@example.com");
        assert_eq!(parsed["pass"], "pw");
        assert_eq!(parsed["to"], "x@y.com");
    }

    #[tokio::test]
    async fn render_body_template_conditional_omits_absent_param() {
        // Regression: calendar_create.yaml had {{description}} leak as literal string
        // when the agent omitted the optional `description` param. Conditional blocks
        // must be stripped if the referenced param is absent.
        use std::collections::HashMap;
        let resolver = MapResolver {
            map: HashMap::new(),
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "summary".to_string(),
            serde_json::Value::String("Meet".to_string()),
        );
        // `description` intentionally absent.

        let template = r#"{"summary":"{{summary}}"{{#if description}},"description":"{{description}}"{{/if}}}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;

        let parsed: serde_json::Value = serde_json::from_str(&rendered)
            .unwrap_or_else(|e| panic!("rendered body is not valid JSON: {e} — got: {rendered}"));
        assert_eq!(parsed["summary"], "Meet");
        assert!(
            parsed.get("description").is_none(),
            "description should be absent, was: {:?}",
            parsed.get("description")
        );
    }

    #[tokio::test]
    async fn render_body_template_conditional_includes_present_param() {
        use std::collections::HashMap;
        let resolver = MapResolver {
            map: HashMap::new(),
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "summary".to_string(),
            serde_json::Value::String("Meet".to_string()),
        );
        params.insert(
            "description".to_string(),
            serde_json::Value::String("Weekly sync".to_string()),
        );

        let template = r#"{"summary":"{{summary}}"{{#if description}},"description":"{{description}}"{{/if}}}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;

        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["summary"], "Meet");
        assert_eq!(parsed["description"], "Weekly sync");
    }

    #[tokio::test]
    async fn search_web_body_template_omits_optional_params_when_absent() {
        // Regression: search_web.yaml body_template must not leak literal {{max_results}}
        // or {{provider}} when the model omits those optional params.
        use std::collections::HashMap;
        let resolver = MapResolver {
            map: HashMap::new(),
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "query".to_string(),
            serde_json::Value::String("rust".to_string()),
        );
        // max_results and provider intentionally absent.

        let template = r#"{"query": "{{query}}"{{#if max_results}}, "max_results": {{max_results}}{{/if}}{{#if provider}}, "provider": "{{provider}}"{{/if}}}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;

        assert!(
            !rendered.contains("{{"),
            "literal template leak in rendered body: {rendered}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&rendered)
            .unwrap_or_else(|e| panic!("rendered body is not valid JSON: {e} — got: {rendered}"));
        assert_eq!(parsed["query"], "rust");
        assert!(
            parsed.get("provider").is_none(),
            "provider should be absent, was: {:?}",
            parsed.get("provider")
        );
        assert!(
            parsed.get("max_results").is_none(),
            "max_results should be absent, was: {:?}",
            parsed.get("max_results")
        );
    }

    #[tokio::test]
    async fn search_web_body_template_includes_all_params_when_present() {
        use std::collections::HashMap;
        let resolver = MapResolver {
            map: HashMap::new(),
        };
        let mut params = serde_json::Map::new();
        params.insert(
            "query".to_string(),
            serde_json::Value::String("rust".to_string()),
        );
        params.insert(
            "max_results".to_string(),
            serde_json::Value::Number(10.into()),
        );
        params.insert(
            "provider".to_string(),
            serde_json::Value::String("searxng".to_string()),
        );

        let template = r#"{"query": "{{query}}"{{#if max_results}}, "max_results": {{max_results}}{{/if}}{{#if provider}}, "provider": "{{provider}}"{{/if}}}"#;
        let rendered = render_body_template(template, &params, Some(&resolver)).await;

        let parsed: serde_json::Value = serde_json::from_str(&rendered)
            .unwrap_or_else(|e| panic!("rendered body is not valid JSON: {e} — got: {rendered}"));
        assert_eq!(parsed["query"], "rust");
        assert_eq!(parsed["max_results"], 10);
        assert_eq!(parsed["provider"], "searxng");
    }

    // ── YAML cache dispatch (wiremock-driven) ───────────────────────────────
    //
    // Verify the cache decisions that `engine_dispatch::execute_tool_call_inner`
    // makes around `YamlToolDef::execute_oauth`. The `dispatch_with_cache`
    // helper mirrors the exact bypass conditions in engine_dispatch:
    //   - skip when `cache:` is absent
    //   - skip when `channel_action:` is set (binary routed elsewhere)
    //   - skip when `pagination:` is set (single-page cache is wrong)
    //   - never cache when execute_oauth returns Err (non-2xx path)
    //
    // Kept inline next to the cache primitives per the lib.rs facade discipline
    // (only leaf modules are re-exported, and `yaml_tools` has `crate::*` deps).

    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn dispatch_with_cache(
        tool: &YamlToolDef,
        args: &serde_json::Value,
        ctx: &ToolExecutionContext,
        http: &reqwest::Client,
    ) -> Result<String, String> {
        let cache_key = match &tool.cache {
            Some(cfg) if tool.channel_action.is_none() && tool.pagination.is_none() => {
                Some(build_cache_key(
                    "test-agent",
                    &tool.name,
                    &tool.method,
                    &tool.endpoint,
                    args,
                    &cfg.key_params,
                ))
            }
            _ => None,
        };

        if let Some(key) = cache_key.as_ref()
            && let Some(body) = ctx.get_cached(key).await
        {
            return Ok(body);
        }

        match tool.execute_oauth(args, http, None, None).await {
            Ok(body) => {
                if let (Some(key), Some(cfg)) = (cache_key.as_ref(), tool.cache.as_ref()) {
                    ctx.set_cached(key, &body, cfg.ttl).await;
                }
                Ok(body)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn yaml_tool(spec: &str) -> YamlToolDef {
        serde_yaml::from_str::<YamlToolDef>(spec).expect("parse tool yaml")
    }

    #[tokio::test]
    async fn cache_hit_skips_http_call() {
        let mock = MockServer::start().await;
        // Exactly one upstream call — the second invocation must hit cache.
        Mock::given(wm_method("GET"))
            .and(wm_path("/v"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": "v1"})),
            )
            .expect(1)
            .mount(&mock)
            .await;

        let tool = yaml_tool(&format!(
            "name: search\ndescription: test\nendpoint: {}/v\nmethod: GET\ncache:\n  ttl: 60\n",
            mock.uri()
        ));

        let ctx = ToolExecutionContext::new(100);
        let http = reqwest::Client::new();
        let args = serde_json::json!({"q": "hello"});

        let r1 = dispatch_with_cache(&tool, &args, &ctx, &http)
            .await
            .expect("call 1");
        let r2 = dispatch_with_cache(&tool, &args, &ctx, &http)
            .await
            .expect("call 2");
        assert_eq!(r1, r2, "second call must return same body from cache");
    }

    #[tokio::test]
    async fn cache_miss_on_distinct_args() {
        let mock = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/v"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": "v1"})),
            )
            .expect(2)
            .mount(&mock)
            .await;

        let tool = yaml_tool(&format!(
            "name: search\ndescription: test\nendpoint: {}/v\nmethod: GET\ncache:\n  ttl: 60\n",
            mock.uri()
        ));

        let ctx = ToolExecutionContext::new(100);
        let http = reqwest::Client::new();
        let _ = dispatch_with_cache(&tool, &serde_json::json!({"q": "a"}), &ctx, &http)
            .await
            .expect("a");
        let _ = dispatch_with_cache(&tool, &serde_json::json!({"q": "b"}), &ctx, &http)
            .await
            .expect("b");
    }

    #[tokio::test]
    async fn non_2xx_response_not_cached() {
        let mock = MockServer::start().await;
        // 500 first, then 200. If 500 were cached the second dispatch
        // wouldn't reach the second mock — the .expect(1) would fail.
        Mock::given(wm_method("GET"))
            .and(wm_path("/v"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&mock)
            .await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/v"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": "ok"})),
            )
            .expect(1)
            .mount(&mock)
            .await;

        let tool = yaml_tool(&format!(
            "name: search\ndescription: test\nendpoint: {}/v\nmethod: GET\ncache:\n  ttl: 60\n",
            mock.uri()
        ));

        let ctx = ToolExecutionContext::new(100);
        let http = reqwest::Client::new();
        let args = serde_json::json!({"q": "x"});
        let r1 = dispatch_with_cache(&tool, &args, &ctx, &http).await;
        assert!(r1.is_err(), "first call returns Err for 500");
        let r2 = dispatch_with_cache(&tool, &args, &ctx, &http)
            .await
            .expect("call 2");
        assert!(
            r2.contains("ok"),
            "second call must hit the 200 branch, not a cached 500"
        );
    }

    #[tokio::test]
    async fn channel_action_bypasses_cache() {
        // channel_action tools route binary output to a channel; their HTTP
        // response is never returned to the LLM. Caching is meaningless.
        let mock = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/v"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x01, 0x02, 0x03]))
            .expect(2)
            .mount(&mock)
            .await;

        let tool = yaml_tool(&format!(
            "name: send_voice\ndescription: test\nendpoint: {}/v\nmethod: GET\ncache:\n  ttl: 60\nchannel_action:\n  action: send_voice\n  data_field: _binary\n",
            mock.uri()
        ));

        let ctx = ToolExecutionContext::new(100);
        let http = reqwest::Client::new();
        let args = serde_json::json!({"text": "hi"});
        let _ = dispatch_with_cache(&tool, &args, &ctx, &http).await;
        let _ = dispatch_with_cache(&tool, &args, &ctx, &http).await;
    }

    #[tokio::test]
    async fn pagination_bypasses_cache() {
        // pagination tools auto-fetch multiple pages mid-execution; caching
        // one page is wrong (the next dispatch would skip pagination).
        let mock = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/v"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})),
            )
            .expect(2)
            .mount(&mock)
            .await;

        let tool = yaml_tool(&format!(
            "name: list_items\ndescription: test\nendpoint: {}/v\nmethod: GET\ncache:\n  ttl: 60\npagination:\n  type: offset\n  param: offset\n  limit_param: limit\n  limit: 20\n  max_pages: 1\n",
            mock.uri()
        ));

        let ctx = ToolExecutionContext::new(100);
        let http = reqwest::Client::new();
        let _ = dispatch_with_cache(&tool, &serde_json::json!({}), &ctx, &http).await;
        let _ = dispatch_with_cache(&tool, &serde_json::json!({}), &ctx, &http).await;
    }
}
