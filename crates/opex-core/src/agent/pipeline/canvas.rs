//! Canvas tool -- present/push_data/clear/run_js/snapshot via browser-renderer.
//!
//! Extracted from `engine_tools.rs` as free functions (no `&self` on `AgentEngine`).

use crate::agent::engine::{CanvasContent, CANVAS_MAX_BYTES};

/// Canvas tool: present/push_data/clear push to UI; run_js/snapshot use browser-renderer.
pub async fn handle_canvas(
    canvas_state: &tokio::sync::RwLock<Option<CanvasContent>>,
    agent_name: &str,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    http_client: &reqwest::Client,
    args: &serde_json::Value,
) -> String {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("present");
    match action {
        "present" | "push_data" => {
            let (ct, content, title) = if action == "push_data" {
                ("json", args.get("content").and_then(|v| v.as_str()).unwrap_or("{}"), None)
            } else {
                (
                    args.get("content_type").and_then(|v| v.as_str()).unwrap_or("markdown"),
                    args.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                    args.get("title").and_then(|v| v.as_str()),
                )
            };
            if content.len() > CANVAS_MAX_BYTES {
                return format!("Error: content too large ({} bytes, max {CANVAS_MAX_BYTES})", content.len());
            }
            *canvas_state.write().await = Some(CanvasContent {
                content_type: ct.to_string(),
                content: content.to_string(),
                title: title.map(|s| s.to_string()),
            });
            let event = opex_types::ws::WsEvent::CanvasUpdate {
                agent: agent_name.to_string(),
                action: action.to_string(),
                content_type: Some(ct.to_string()),
                content: Some(content.to_string()),
                title: title.map(std::string::ToString::to_string),
            };
            if let Some(tx) = ui_event_tx {
                tx.send(event.to_json()).ok();
            }
            "Canvas updated".to_string()
        }
        "clear" => {
            *canvas_state.write().await = None;
            let event = opex_types::ws::WsEvent::CanvasUpdate {
                agent: agent_name.to_string(),
                action: "clear".to_string(),
                content_type: None,
                content: None,
                title: None,
            };
            if let Some(tx) = ui_event_tx {
                tx.send(event.to_json()).ok();
            }
            "Canvas cleared".to_string()
        }
        "run_js" => {
            let code = match args.get("code").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => return "Error: 'code' parameter is required for run_js".to_string(),
            };
            canvas_run_js(canvas_state, http_client, code).await
        }
        "snapshot" => {
            canvas_snapshot(canvas_state, http_client).await
        }
        other => format!("Unknown canvas action: {other}"),
    }
}

/// POST JSON to browser-renderer and return the parsed response body.
pub async fn br_post(
    http_client: &reqwest::Client,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let br_url = browser_renderer_url();
    let resp = http_client
        .post(format!("{br_url}{path}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Cannot reach browser-renderer: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("browser-renderer {status}: {body}"));
    }
    resp.json::<serde_json::Value>().await
        .map_err(|e| format!("Error parsing browser-renderer response: {e}"))
}

/// Resolve the navigable URL for the current canvas content.
pub async fn canvas_resolve_url(
    canvas_state: &tokio::sync::RwLock<Option<CanvasContent>>,
) -> Result<String, String> {
    let state_guard = canvas_state.read().await;
    let state = state_guard.as_ref()
        .ok_or_else(|| "Error: no content on canvas. Use canvas(action='present') first.".to_string())?
        .clone();
    drop(state_guard);
    canvas_content_url(&state)
}

/// Get a URL that browser-renderer can navigate to for the current canvas content.
///
/// For `content_type = "url"`, the URL is LLM-controlled. Audit 2026-05-08
/// found that `canvas_run_js` and `canvas_snapshot` passed this URL straight
/// to browser-renderer, which would happily fetch internal services
/// (gateway API, postgres on 5434, toolgate, etc.). We now run the same
/// SSRF pre-check used by YAML tools — `validate_url_scheme` rejects
/// non-http(s) schemes, internal-blocklist authorities, and numeric private
/// IPs; the browser-renderer's HTTP client also runs through the
/// `SsrfSafeResolver` for DNS-level filtering.
pub fn canvas_content_url(state: &CanvasContent) -> Result<String, String> {
    match state.content_type.as_str() {
        "url" => {
            crate::net::ssrf::validate_url_scheme(&state.content)
                .map_err(|e| format!(
                    "Error: canvas URL '{}' rejected as unsafe: {e}",
                    state.content,
                ))?;
            Ok(state.content.clone())
        }
        "html" => {
            use base64::Engine;
            Ok(format!(
                "data:text/html;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&state.content)
            ))
        }
        other => Err(format!(
            "Error: run_js/snapshot not supported for content_type '{other}'. Use 'html' or 'url'."
        )),
    }
}

/// Resolve browser-renderer service URL.
pub fn browser_renderer_url() -> String {
    std::env::var("BROWSER_RENDERER_URL")
        .unwrap_or_else(|_| "http://localhost:9020".to_string())
}

/// Execute JavaScript in the current canvas content via browser-renderer.
async fn canvas_run_js(
    canvas_state: &tokio::sync::RwLock<Option<CanvasContent>>,
    http_client: &reqwest::Client,
    code: &str,
) -> String {
    let url = match canvas_resolve_url(canvas_state).await {
        Ok(u) => u,
        Err(e) => return e,
    };

    let session_id = match br_post(http_client, "/automation", serde_json::json!({"action": "create_session"})).await {
        Ok(v) => v.get("session_id").and_then(|s| s.as_str()).unwrap_or("").to_string(),
        Err(e) => return e,
    };

    if let Err(e) = br_post(http_client, "/automation", serde_json::json!({
        "action": "navigate", "session_id": session_id, "url": url, "timeout": 15,
    })).await {
        let _ = br_post(http_client, "/automation", serde_json::json!({"action": "close", "session_id": session_id})).await;
        return format!("Error navigating: {e}");
    }

    let result = match br_post(http_client, "/automation", serde_json::json!({
        "action": "evaluate", "session_id": session_id, "js": code,
    })).await {
        Ok(v) => {
            let res = &v["result"];
            if res.is_string() { res.as_str().unwrap().to_string() }
            else { serde_json::to_string(res).unwrap_or_default() }
        }
        Err(e) => format!("JS execution error: {e}"),
    };

    let _ = br_post(http_client, "/automation", serde_json::json!({"action": "close", "session_id": session_id})).await;
    result
}

/// Take a screenshot of the current canvas content via browser-renderer.
async fn canvas_snapshot(
    canvas_state: &tokio::sync::RwLock<Option<CanvasContent>>,
    http_client: &reqwest::Client,
) -> String {
    let url = match canvas_resolve_url(canvas_state).await {
        Ok(u) => u,
        Err(e) => return e,
    };

    let br_url = browser_renderer_url();
    let resp = match http_client
        .post(format!("{br_url}/screenshot"))
        .json(&serde_json::json!({"url": url, "timeout": 15}))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("Cannot reach browser-renderer: {e}"),
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return format!("browser-renderer {status}: {body}");
    }
    let len = resp.content_length().unwrap_or(0);
    let _ = resp.bytes().await;
    format!("Screenshot captured (PNG, {len} bytes).")
}
