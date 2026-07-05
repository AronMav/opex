//! Codemode (tools-as-code) loopback endpoints.
//!
//! Two loopback-only endpoints that let a script running inside the code_exec
//! sandbox call back into core to invoke tools and search the tool catalog.
//! The security boundary is the `X-Codemode-Token` HMAC (verified per call),
//! not the bearer auth token — the middleware exempts these paths from auth
//! when the request originates from loopback (see `middleware.rs`).
//!
//! - `POST /api/sandbox/tool-call` — dispatch a single tool by name. Reuses
//!   the engine's `SystemToolRegistry::dispatch` + `ToolDeps::from_engine` so
//!   the same policy/deny checks apply as in the LLM tool loop.
//! - `POST /api/sandbox/tool-search` — substring search over the agent's
//!   visible tool definitions (progressive disclosure for large tool catalogs).
//!
//! Capability tokens are minted by the `code_orchestrate` system tool and bound
//! to `session_id` + `agent_name` + `tools_hash` (a hash of the sorted
//! allowed-tools list) with a short TTL scoped to one codemode run.
//! See `uploads::mint_codemode_token`.

use std::collections::HashSet;
use std::sync::OnceLock;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::agent::tool_registry::ToolDeps;
use crate::gateway::state::AppState;
use crate::uploads::{codemode_tools_hash, verify_codemode_token};

/// Request body for `POST /api/sandbox/tool-call`.
#[derive(Debug, Deserialize)]
pub struct ToolCallRequest {
    pub tool: String,
    pub arguments: serde_json::Value,
    pub _ctx: CallCtx,
}

/// Context identifying the codemode run (bound to the capability token).
#[derive(Debug, Deserialize)]
pub struct CallCtx {
    pub session_id: uuid::Uuid,
    pub agent_name: String,
    /// The call index within the script (for nested tool-call SSE events).
    /// Unused by the endpoint itself but required so the SDK stub includes it
    /// in the request body — the code_orchestrate handler parses it from logs.
    #[allow(dead_code)]
    pub call_index: u32,
}

/// Response body for `POST /api/sandbox/tool-call`.
#[derive(Debug, Serialize)]
pub struct ToolCallResponse {
    pub result: String,
}

/// Error response body.
#[derive(Debug, Serialize)]
pub struct ToolCallError {
    pub error: String,
    /// `"approval_required"` when the tool needs interactive approval (which
    /// codemode can't provide — the script must handle this).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Request body for `POST /api/sandbox/tool-search`.
#[derive(Debug, Deserialize)]
pub struct ToolSearchRequest {
    pub query: String,
    #[serde(default)]
    pub limit: Option<usize>,
    pub _ctx: CallCtx,
}

/// One search result.
#[derive(Debug, Serialize)]
pub struct ToolSearchResult {
    pub name: String,
    pub description: String,
    pub signature: serde_json::Value,
}

/// Response body for `POST /api/sandbox/tool-search`.
#[derive(Debug, Serialize)]
pub struct ToolSearchResponse {
    pub items: Vec<ToolSearchResult>,
    pub total: usize,
}

pub(crate) fn routes() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/api/sandbox/tool-call", post(tool_call))
        .route("/api/sandbox/tool-search", post(tool_search))
}

/// Maximum number of search results returned.
const SEARCH_DEFAULT_LIMIT: usize = 10;

/// Process-global concurrency limit for codemode tool dispatch (M2). Prevents
/// a script using `concurrent.futures.ThreadPoolExecutor` from exhausting DB
/// connections, memory, or CPU with dozens of parallel tool calls. Matches the
/// main LLM tool-loop semaphore (default 10).
static CODEMODE_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

fn codemode_semaphore() -> &'static Semaphore {
    CODEMODE_SEMAPHORE.get_or_init(|| Semaphore::new(10))
}

/// Dispatch a tool call from a sandbox script.
async fn tool_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ToolCallRequest>,
) -> Result<Json<ToolCallResponse>, (StatusCode, Json<ToolCallError>)> {
    let key = state.infra.secrets.get_upload_hmac_key();

    let token = headers
        .get("x-codemode-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if token.is_empty() {
        return Err(tool_err(StatusCode::UNAUTHORIZED, "missing X-Codemode-Token", None));
    }

    let agent_name = &req._ctx.agent_name;
    let session_id = req._ctx.session_id;

    // Resolve the agent engine BEFORE token verification. We need the engine
    // to re-derive tools_hash for the HMAC check. To avoid an agent-name
    // oracle (M1), we return a generic 401 for both "agent not found" and
    // "invalid token" — a caller without a valid token cannot distinguish.
    let engine = {
        let map = state.agents.map.read().await;
        match map.get(agent_name) {
            Some(h) => h.engine.clone(),
            None => {
                return Err(tool_err(
                    StatusCode::UNAUTHORIZED,
                    "invalid or expired X-Codemode-Token",
                    None,
                ));
            }
        }
    };

    // Build the available tools set for this agent (filtered by policy).
    // Exclude code_orchestrate itself to prevent recursive codemode calls
    // (H2 — a script calling tools.code_orchestrate(...) would nest docker
    // execs with no depth limit, exhausting container resources).
    let available: HashSet<String> = engine
        .codemode_available_tool_names()
        .into_iter()
        .filter(|name| name != "code_orchestrate")
        .collect();
    let tools_hash = codemode_tools_hash(
        &available.iter().cloned().collect::<Vec<_>>(),
    );

    if !verify_codemode_token(&key, session_id, agent_name, tools_hash, token) {
        return Err(tool_err(
            StatusCode::UNAUTHORIZED,
            "invalid or expired X-Codemode-Token (agent tool list may have changed)",
            None,
        ));
    }

    // Check the requested tool is in the available set.
    if !available.contains(&req.tool) {
        return Err(tool_err(
            StatusCode::FORBIDDEN,
            &format!("tool '{}' is not available for agent '{}'", req.tool, agent_name),
            None,
        ));
    }

    // Approval check (H1): codemode runs non-interactively — there is no human
    // to approve a sensitive tool call. If the tool requires approval, reject
    // with kind="approval_required" so the script can handle it gracefully.
    if engine.needs_approval(&req.tool) {
        return Err(tool_err(
            StatusCode::FORBIDDEN,
            &format!(
                "tool '{}' requires interactive approval, which is not supported in codemode",
                req.tool
            ),
            Some("approval_required"),
        ));
    }

    // Build ToolDeps from the engine + dispatch via the system tool registry.
    // Acquire a concurrency permit so a script using ThreadPoolExecutor can't
    // exhaust resources with dozens of parallel calls (M2).
    let _permit = codemode_semaphore().acquire().await
        .map_err(|_| tool_err(StatusCode::INTERNAL_SERVER_ERROR, "codemode semaphore closed", None))?;
    let deps = ToolDeps::from_engine(&engine, &available, Some(session_id));
    let result = engine
        .tool_registry()
        .dispatch(&req.tool, &deps, &req.arguments)
        .await;

    match result {
        Some(output) => Ok(Json(ToolCallResponse { result: output })),
        None => Err(tool_err(
            StatusCode::NOT_FOUND,
            &format!(
                "tool '{}' is not a system tool (codemode v1 supports system tools only)",
                req.tool
            ),
            None,
        )),
    }
}

/// Search the agent's visible tool definitions by substring.
async fn tool_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ToolSearchRequest>,
) -> Result<Json<ToolSearchResponse>, (StatusCode, Json<ToolCallError>)> {
    let key = state.infra.secrets.get_upload_hmac_key();
    let token = headers
        .get("x-codemode-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if token.is_empty() {
        return Err(tool_err(StatusCode::UNAUTHORIZED, "missing X-Codemode-Token", None));
    }

    let agent_name = &req._ctx.agent_name;
    let session_id = req._ctx.session_id;

    // Resolve the agent engine BEFORE token verification (M1: return generic
    // 401 for both "agent not found" and "invalid token").
    let engine = {
        let map = state.agents.map.read().await;
        match map.get(agent_name) {
            Some(h) => h.engine.clone(),
            None => {
                return Err(tool_err(
                    StatusCode::UNAUTHORIZED,
                    "invalid or expired X-Codemode-Token",
                    None,
                ));
            }
        }
    };

    // Same tools_hash computation as tool_call: filter out code_orchestrate.
    let available: HashSet<String> = engine
        .codemode_available_tool_names()
        .into_iter()
        .filter(|name| name != "code_orchestrate")
        .collect();
    let tools_hash = codemode_tools_hash(
        &available.iter().cloned().collect::<Vec<_>>(),
    );

    if !verify_codemode_token(&key, session_id, agent_name, tools_hash, token) {
        return Err(tool_err(StatusCode::UNAUTHORIZED, "invalid or expired X-Codemode-Token", None));
    }

    let tools = engine.tool_definitions_for_search();
    let query_lower = req.query.to_lowercase();
    let limit = req.limit.unwrap_or(SEARCH_DEFAULT_LIMIT).min(50);

    let mut results: Vec<(i64, ToolSearchResult)> = Vec::new();
    for tool in &tools {
        if !available.contains(&tool.name) {
            continue;
        }
        let name_lower = tool.name.to_lowercase();
        let desc_lower = tool.description.to_lowercase();
        let mut score: i64 = 0;
        if name_lower == query_lower {
            score += 20;
        } else if name_lower.contains(&query_lower) {
            score += 8;
        }
        if desc_lower.contains(&query_lower) {
            score += 4;
        }
        if let Some(props) = tool.input_schema.get("properties").and_then(|p| p.as_object()) {
            for (prop_name, prop_schema) in props {
                let pn = prop_name.to_lowercase();
                if pn.contains(&query_lower) {
                    score += 2;
                }
                if let Some(desc) = prop_schema.get("description").and_then(|d| d.as_str())
                    && desc.to_lowercase().contains(&query_lower)
                {
                    score += 2;
                }
            }
        }
        if score > 0 || query_lower.is_empty() {
            results.push((
                score,
                ToolSearchResult {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    signature: tool.input_schema.clone(),
                },
            ));
        }
    }

    results.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    let total = results.len();
    let items: Vec<ToolSearchResult> = results.into_iter().take(limit).map(|(_, r)| r).collect();

    Ok(Json(ToolSearchResponse { items, total }))
}

/// Build a typed error response.
fn tool_err(status: StatusCode, msg: &str, kind: Option<&str>) -> (StatusCode, Json<ToolCallError>) {
    (status, Json(ToolCallError {
        error: msg.to_string(),
        kind: kind.map(String::from),
    }))
}