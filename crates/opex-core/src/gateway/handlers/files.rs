//! File Handler Hub — core orchestration routes (sync path).
//!
//! `POST /api/files/run` (and its token-scoped sibling `/api/files/menu-run`,
//! plus `/api/commands/menu-run`) re-check the tiered gate server-side,
//! download the upload bytes over LOOPBACK (in Rust), POST them as
//! multipart/form-data to toolgate `/handlers/{id}/run`, then persist the
//! result as a provenance-wrapped `source='file_handler'` message and return
//! the outcome in the POST body (the chat-delivery path). Async jobs report
//! back via `/api/files/jobs/{job_id}/progress` + `/complete`. Produced
//! artifacts are also broadcast best-effort on the GLOBAL `ui_event_tx` bus.
//!
//! Toolgate never receives a loopback URL (its SSRF guard would reject it) —
//! mirrors `dispatch.rs::run_transcribe` (R12, SSRF×loopback note).
//!
//! Chat-delivery note (R-CHAT): the POST `/run` response body IS the
//! chat-delivery path (it returns the full `ScenarioOutcome` to the composer).
//! The `ui_event_tx.send(...)` broadcast on the success path uses the GLOBAL
//! UI WebSocket event bus (the same channel that carries `session_updated` /
//! `notification`), NOT the per-session chat SSE stream. It is therefore a
//! best-effort cross-surface notification, and may be a no-op until a UI
//! consumer for a global `type:"file"` ui_event is wired in Phase 4.

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::agent::file_scenario::outcome::{ScenarioOutcome, ScenarioStatus};
use crate::agent::handler_registry::{HandlerRegistry, match_buttons, match_url_handlers};
use crate::gateway::AppState;
use crate::gateway::clusters::InfraServices;
use opex_db::handler_jobs;

// ── path-traversal allowlist ──────────────────────────────────────────────────

/// Compiled once. Allows filenames and folder names that contain only
/// `A-Za-z0-9 _.-` (1–128 chars). No slashes, no backslashes, no `..`.
/// This is the FIX 2 traversal guard for `run_post_action`.
static SAFE_FILENAME_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

fn safe_filename_re() -> &'static regex::Regex {
    SAFE_FILENAME_RE.get_or_init(|| {
        regex::Regex::new(r"^[A-Za-z0-9 _.\-]{1,128}$").expect("static regex is valid")
    })
}

/// Returns `true` iff `name` passes the post_action path traversal allowlist.
/// Exported `pub(crate)` so the inline test module can exercise it directly.
///
/// Two-layer check:
/// 1. Explicit rejection of lone `.` / `..` (these match the character class).
/// 2. Regex `^[A-Za-z0-9 _.-]{1,128}$` — no slashes, no backslashes.
pub(crate) fn is_safe_path_component(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    safe_filename_re().is_match(name)
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/files/run", post(run_menu_handler))
        .route("/api/files/menu-run", post(run_menu_token_handler))
        .route("/api/commands/menu-run", post(command_menu_run))
        .route("/api/files/jobs/{job_id}/progress", post(job_progress))
        .route("/api/files/jobs/{job_id}/complete", post(job_complete))
}

/// Request body for `POST /api/files/run` — the click-run from a `handler_menu`
/// card button. Deterministic (no LLM round-trip): validates the chosen handler
/// against the matched set for the source, then enqueues a `handler_jobs` row.
#[derive(Deserialize)]
struct MenuRunRequest {
    handler_id: String,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    upload_id: Option<Uuid>,
    session_id: Uuid,
    agent: String,
}

/// Builds the `handler_jobs` params object: base `{"language": lang}` merged
/// with the caller-supplied `extra` object fields (e.g. a chosen choice-valve
/// value from `/api/commands/menu-run`). `extra` must be a JSON object — any
/// non-object value (including the default `json!({})`) contributes nothing
/// beyond the base language field.
fn merge_job_params(lang: &str, extra: &serde_json::Value) -> serde_json::Value {
    let mut params = json!({ "language": lang });
    if let (Some(base), Some(extra_obj)) = (params.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            base.insert(k.clone(), v.clone());
        }
    }
    params
}

/// Shared validate+enqueue for a menu run (web card `/api/files/run`, the
/// Telegram callback `/api/files/menu-run`, and the choice-valve completion
/// `/api/commands/menu-run`). Ownership guard + matched-set check (the same
/// security boundary as the `file_handler` tool's `run`), then enqueue.
/// `extra_params` is merged into the job params (e.g. a chosen valve value) —
/// pass `json!({})` when there is nothing to add.
#[allow(clippy::too_many_arguments)]
async fn menu_run_core(
    infra: &InfraServices,
    handlers: &HandlerRegistry,
    source_url: Option<&str>,
    upload_id_req: Option<Uuid>,
    session_id: Uuid,
    agent: &str,
    handler_id: &str,
    extra_params: serde_json::Value,
) -> axum::response::Response {
    // Ownership guard: the target session must belong to the calling agent
    // (mirrors the session-tool IDOR scoping; OPEX is single-token).
    let session_owner: Option<String> =
        sqlx::query_scalar("SELECT agent_id FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&infra.db)
            .await
            .ok()
            .flatten();
    match session_owner {
        Some(a) if a == agent => {}
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "session does not belong to this agent" })),
            )
                .into_response();
        }
    }

    handlers.refresh().await;
    let manifests = handlers.manifests().await;
    let enabled = crate::agent::fse::get_enabled_allowlist(&infra.db).await;
    let lang = "ru"; // label localization only; matching is language-agnostic

    let source_url = source_url.filter(|s| !s.is_empty());
    let (buttons, upload_id) = if let Some(url) = source_url {
        (match_url_handlers(&manifests, url, &enabled, lang), None)
    } else if let Some(uid) = upload_id_req {
        match assert_upload_accessible(&infra.db, uid).await {
            Ok(meta) => {
                // F070: the menu enqueues onto the async-only handler_jobs queue.
                // Drop sync handlers so a sync click can't be enqueued+stranded
                // (mirrors the match_url_handlers async filter above).
                let mut b = match_buttons(&manifests, &meta.mime, meta.size, &enabled, lang);
                crate::agent::handler_registry::retain_async_handlers(&mut b, &manifests);
                (b, Some(uid))
            }
            Err((status, body)) => return (status, body).into_response(),
        }
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "provide source_url or upload_id" })),
        )
            .into_response();
    };

    if !buttons.iter().any(|b| b.id == handler_id) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "handler not available for this source" })),
        )
            .into_response();
    }

    let params = merge_job_params(lang, &extra_params);
    match handler_jobs::insert_handler_job(
        &infra.db, upload_id, source_url, handler_id, agent, session_id, &params,
    )
    .await
    {
        Ok(job_id) => Json(json!({ "ok": true, "job_id": job_id })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /api/files/run` — the web `handler_menu` card button click.
async fn run_menu_handler(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    Json(req): Json<MenuRunRequest>,
) -> impl IntoResponse {
    menu_run_core(
        &infra,
        &handlers,
        req.source_url.as_deref(),
        req.upload_id,
        req.session_id,
        &req.agent,
        &req.handler_id,
        json!({}),
    )
    .await
}

// ── Menu context store (Telegram callback_data is ≤64 bytes) ──────────────────

/// Short-lived `token → handler_menu params`. A Telegram inline button can only
/// carry `hm:<token>:<handler_id>` (callback_data limit), so the
/// source_url/upload_id/session/agent are stashed here when the menu is sent and
/// recovered on the callback via `/api/files/menu-run`.
type MenuCtxMap = std::collections::HashMap<String, (serde_json::Value, std::time::Instant)>;
static MENU_CTX: std::sync::OnceLock<std::sync::Mutex<MenuCtxMap>> = std::sync::OnceLock::new();

fn menu_ctx() -> &'static std::sync::Mutex<MenuCtxMap> {
    MENU_CTX.get_or_init(|| std::sync::Mutex::new(MenuCtxMap::new()))
}

/// Stash `handler_menu` params under a fresh short token (prunes entries older
/// than 30 min). Called from the channel path when a menu is sent to Telegram.
pub(crate) fn store_menu_ctx(params: serde_json::Value) -> String {
    // Full 122-bit UUID (32 hex) — unguessable capability token. Fits Telegram's
    // 64-byte callback_data as `hm:<32hex>:<handler_id>` (~51 bytes).
    let token: String = uuid::Uuid::new_v4().simple().to_string();
    if let Ok(mut map) = menu_ctx().lock() {
        let now = std::time::Instant::now();
        map.retain(|_, (_, t)| now.duration_since(*t) < std::time::Duration::from_secs(1800));
        map.insert(token.clone(), (params, now));
    }
    token
}

#[derive(Deserialize)]
struct MenuTokenRunRequest {
    token: String,
    handler_id: String,
    /// The chat the callback came from — must match the chat the menu was sent to.
    #[serde(default)]
    chat_id: Option<serde_json::Value>,
}

/// `POST /api/files/menu-run` — the Telegram inline-button callback. Recovers the
/// menu params by token, then runs exactly like `/api/files/run`.
async fn run_menu_token_handler(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    Json(req): Json<MenuTokenRunRequest>,
) -> impl IntoResponse {
    let params = menu_ctx()
        .lock()
        .ok()
        .and_then(|m| m.get(&req.token).map(|(v, _)| v.clone()));
    let Some(params) = params else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "menu expired or unknown" })),
        )
            .into_response();
    };
    // Origin binding: if the menu was bound to a chat, the caller must present
    // the matching chat_id (blocks replay of a leaked token from another chat).
    if let Some(stored_chat) = params.get("_chat_id")
        && req.chat_id.as_ref() != Some(stored_chat)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "callback chat does not match the menu" })),
        )
            .into_response();
    }
    let source_url = params.get("source_url").and_then(|v| v.as_str());
    let upload_id = params
        .get("upload_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let Some(session_id) = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "stored menu missing session" })),
        )
            .into_response();
    };
    let agent = params.get("agent").and_then(|v| v.as_str()).unwrap_or("");
    menu_run_core(
        &infra,
        &handlers,
        source_url,
        upload_id,
        session_id,
        agent,
        &req.handler_id,
        json!({}),
    )
    .await
}

// ── Command choice-valve completion ────────────────────────────────────────────

/// Request body for `POST /api/commands/menu-run` — completes a slash-command
/// invocation whose handler exposes a choice-valve (Task 2's `command_args_menu`
/// card). Mirrors `MenuTokenRunRequest` but carries the chosen `value` instead of
/// a `handler_id` (the handler is recovered from the stash, same as the valve
/// name and everything else needed to re-run the trust gate).
#[derive(Deserialize)]
struct CommandMenuRunRequest {
    token: String,
    value: String,
    #[serde(default)]
    chat_id: Option<serde_json::Value>,
}

/// `POST /api/commands/menu-run` — recovers the stash by `token`, validates
/// `value` against the stashed `choices`, and enqueues via `menu_run_core` (same
/// ownership + matched-set trust gate as every other menu-run path) with the
/// chosen valve merged into the job params.
async fn command_menu_run(
    State(infra): State<InfraServices>,
    State(handlers): State<HandlerRegistry>,
    Json(req): Json<CommandMenuRunRequest>,
) -> impl IntoResponse {
    let ctx = menu_ctx()
        .lock()
        .ok()
        .and_then(|m| m.get(&req.token).map(|(v, _)| v.clone()));
    let Some(ctx) = ctx else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "expired or unknown token" })),
        )
            .into_response();
    };

    // Origin binding, mirroring `run_menu_token_handler`: if the menu was bound
    // to a chat, the caller must present the matching chat_id.
    if let Some(stored_chat) = ctx.get("_chat_id")
        && req.chat_id.as_ref() != Some(stored_chat)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "callback chat does not match the menu" })),
        )
            .into_response();
    }

    let choices: Vec<String> = ctx
        .get("choices")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if !choices.iter().any(|c| c == &req.value) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid choice" })),
        )
            .into_response();
    }

    let handler_id = ctx.get("handler_id").and_then(|v| v.as_str()).unwrap_or_default();
    let agent = ctx.get("agent").and_then(|v| v.as_str()).unwrap_or_default();
    let valve = ctx.get("valve").and_then(|v| v.as_str()).unwrap_or_default();
    let source_url = ctx.get("source_url").and_then(|v| v.as_str());
    let upload_id = ctx
        .get("upload_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let Some(session_id) = ctx
        .get("session_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "stored menu missing session" })),
        )
            .into_response();
    };

    let extra = json!({ valve: req.value });
    menu_run_core(
        &infra,
        &handlers,
        source_url,
        upload_id,
        session_id,
        agent,
        handler_id,
        extra,
    )
    .await
}

// ── Owner-gate ────────────────────────────────────────────────────────────────

/// Minimal upload facts the owner-gate proves before any handler runs.
#[derive(Debug, Clone)]
pub(crate) struct UploadMeta {
    pub mime: String,
    pub size: u64,
}

/// R3 owner-gate (single-tenant v1): the upload must exist and be one of the
/// user-facing owner types (`client_upload` or `tool_output`). Existence + type
/// is the full gate for v1; per-user ACL is deferred to multi-tenant follow-up.
/// Returns `UploadMeta{mime, size}` so the row is read exactly once.
pub(crate) async fn assert_upload_accessible(
    db: &sqlx::PgPool,
    upload_id: Uuid,
) -> Result<UploadMeta, (StatusCode, Json<Value>)> {
    // Scalar query for `owner_type`: avoids fetching the BYTEA `data` column.
    let owner_type: Option<String> = sqlx::query_scalar(
        r#"SELECT owner_type FROM uploads
           WHERE id = $1 AND (expires_at IS NULL OR expires_at > NOW())"#,
    )
    .bind(upload_id)
    .fetch_optional(db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, upload_id = %upload_id, "assert_upload_accessible: owner_type lookup failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "database error"})),
        )
    })?;

    let owner_type = match owner_type {
        Some(t) => t,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": "upload not found"})),
            ));
        }
    };

    if owner_type != "client_upload" && owner_type != "tool_output" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "upload not accessible"})),
        ));
    }

    // Now fetch mime+size (no BYTEA) for the caller.
    let row = crate::db::uploads::get_by_id(db, upload_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, upload_id = %upload_id, "assert_upload_accessible: row lookup failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "database error"})),
            )
        })?
        .ok_or((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "upload not found"})),
        ))?;

    Ok(UploadMeta {
        mime: row.mime,
        size: row.size_bytes.max(0) as u64,
    })
}

// ── Async-job callback types ───────────────────────────────────────────────────

/// Body for `POST /api/files/jobs/{job_id}/progress`.
#[derive(Debug, Deserialize)]
pub(crate) struct JobProgressBody {
    pub phase: String,
    pub pct: i32,
}

// ── Async-job callback helpers ────────────────────────────────────────────────

/// Generic WS event broadcast on every async-job progress/terminal step.
/// Generalization of `video_progress` (the queue is handler-agnostic).
pub(crate) fn file_job_progress_event(
    job_id: &str,
    handler_id: &str,
    session_id: &str,
    phase: &str,
    pct: i32,
    status: &str,
) -> opex_types::ws::WsEvent {
    opex_types::ws::WsEvent::FileJobProgress {
        job_id: job_id.to_string(),
        handler_id: handler_id.to_string(),
        session_id: session_id.to_string(),
        phase: phase.to_string(),
        pct,
        status: status.to_string(),
    }
}

// ── Async-job callback endpoints ──────────────────────────────────────────────

/// `POST /api/files/jobs/{job_id}/progress`
///
/// Internal callback posted by the async runner to report incremental progress.
/// Auth is required (same Bearer token as all other /api routes — the runner has it).
/// Additionally, the per-job HMAC token in `X-Job-Token` must verify (FIX 1 IDOR guard).
/// Updates the `handler_jobs` row and broadcasts a `file_job_progress` WS event.
async fn job_progress(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<JobProgressBody>,
) -> StatusCode {
    // FIX 1: per-job HMAC token — reject if missing or invalid.
    let key = state.infra.secrets.get_upload_hmac_key();
    let token = headers
        .get("x-job-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !crate::uploads::verify_job_callback_token(&key, job_id, token) {
        tracing::warn!(%job_id, "job_progress: missing or invalid X-Job-Token");
        return StatusCode::UNAUTHORIZED;
    }

    let db = &state.infra.db;
    if let Err(e) = handler_jobs::update_handler_job_progress(db, job_id, &body.phase, body.pct).await {
        tracing::warn!(error = %e, %job_id, "job_progress: db update failed");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    if let Ok(Some(job)) = handler_jobs::get_handler_job(db, job_id).await {
        let ev = file_job_progress_event(
            &job_id.to_string(),
            &job.handler_id,
            &job.session_id.to_string(),
            &body.phase,
            body.pct,
            "processing",
        );
        let _ = state.channels.ui_event_tx.send(ev.to_json());
    }
    StatusCode::NO_CONTENT
}

/// `POST /api/files/jobs/{job_id}/complete`
///
/// Internal callback posted by the async runner with the final `ScenarioOutcome`.
/// Auth is required (same Bearer token as all other /api routes).
/// Additionally, the per-job HMAC token in `X-Job-Token` must verify (FIX 1 IDOR guard).
/// On success: marks the job done, persists a provenance-wrapped `source='file_handler'`
/// message, runs the optional `post_action` (MCP vault write), emits a final WS event.
/// On failure status: marks the job failed, emits a terminal WS event.
async fn job_complete(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(outcome): Json<ScenarioOutcome>,
) -> StatusCode {
    // FIX 1: per-job HMAC token — reject if missing or invalid.
    let key = state.infra.secrets.get_upload_hmac_key();
    let token = headers
        .get("x-job-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !crate::uploads::verify_job_callback_token(&key, job_id, token) {
        tracing::warn!(%job_id, "job_complete: missing or invalid X-Job-Token");
        return StatusCode::UNAUTHORIZED;
    }

    let db = &state.infra.db;
    let job = match handler_jobs::get_handler_job(db, job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => return StatusCode::NOT_FOUND,
        Err(e) => {
            tracing::warn!(error = %e, %job_id, "job_complete: load failed");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    let is_ok = matches!(outcome.status, ScenarioStatus::Ok);
    let terminal = if is_ok { "done" } else { "failed" };

    // ScenarioOutcome now carries post_action (default + skip_serializing_if),
    // so this re-serialization PRESERVES the handler's vault-write request into
    // the stored result JSON for run_post_action to read back.
    let result_json = serde_json::to_value(&outcome).unwrap_or_else(|_| serde_json::json!({}));

    if is_ok {
        // Atomic transition: only proceed with side effects if the row moved
        // from 'processing' → 'done'. A false return means the job is already
        // terminal (e.g. replayed callback) — skip deliver to avoid duplicates.
        let transitioned = match handler_jobs::mark_handler_job_done(db, job_id, &result_json).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, %job_id, "job_complete: mark_handler_job_done failed");
                false
            }
        };
        if transitioned {
            deliver_async_outcome(&state, &job, &outcome).await;
        } else {
            tracing::info!(%job_id, "job_complete: already terminal — skipping duplicate deliver");
            return StatusCode::NO_CONTENT;
        }
    } else {
        let reason = outcome.reason.clone().unwrap_or_else(|| "handler failed".to_string());
        // Atomic transition: skip the terminal WS event if already terminal.
        let transitioned = match handler_jobs::mark_handler_job_failed(db, job_id, &reason).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, %job_id, "job_complete: mark_handler_job_failed failed");
                false
            }
        };
        if !transitioned {
            tracing::info!(%job_id, "job_complete: already terminal — skipping duplicate failed event");
            return StatusCode::NO_CONTENT;
        }
        // Surface the failure IN the chat (not just a transient WS event) so the
        // user isn't left staring at "…готовлю сводку" with no trace on reload.
        // Must run BEFORE the terminal WS event below — the UI refetches session
        // messages on `status=="failed"`, so the row has to exist first.
        deliver_async_failure(&state, &job, &reason).await;
    }

    let ev = file_job_progress_event(
        &job_id.to_string(),
        &job.handler_id,
        &job.session_id.to_string(),
        "done",
        100,
        terminal,
    );
    let _ = state.channels.ui_event_tx.send(ev.to_json());
    StatusCode::NO_CONTENT
}

/// Persist the async outcome as a file-derived assistant message (R4/R8:
/// provenance-wrapped content, source='file_handler', no explicit status),
/// run the generic post-completion action (MCP/Obsidian vault write), and
/// emit the terminal `file_job_progress` WS event so the UI reacts without reload.
/// Called from `job_complete` only when the `handler_jobs` row actually
/// transitioned from `'processing'` → `'done'` (guarded by `file_handler_worker`
/// / `handler_jobs` idempotency — no duplicate deliver on replayed callbacks).
async fn deliver_async_outcome(
    state: &AppState,
    job: &handler_jobs::HandlerJob,
    outcome: &ScenarioOutcome,
) {
    // 1. Generic post-action FIRST (before persist) so the success message can
    //    report WHERE the note landed. The handler may request a direct note
    //    write via a `post_action` object in the result JSON (no mcp-obsidian
    //    dependency). Returns the workspace-relative path, or None if nothing was
    //    written.
    let saved = run_post_action(job.id, outcome).await;

    // 2. Provenance-wrap with the REAL handler_id + upload_id (R4). URL-based
    //    jobs (no upload) carry an empty upload id in the wrapper.
    let upload_id = job.upload_id.map(|u| u.to_string()).unwrap_or_default();
    let wrapped = crate::agent::provenance::wrap_file_output(
        &job.handler_id,
        &upload_id,
        &outcome.summary_text,
    );

    // 3. Human-facing success header — mirrors deliver_async_failure's "⚠️ …"
    //    prefix so the chat always carries a clear terminal message (the raw
    //    provenance wrapper below is stripped for display by the UI, and kept
    //    only so the LLM treats the file-derived text as untrusted next turn).
    let header = match &saved {
        Some(path) => format!("✅ Готово — {}. Сохранено: `{}`", job.handler_id, path),
        None => format!("✅ Готово — {}.", job.handler_id),
    };
    let content = format!("{header}\n\n{wrapped}");

    // 4. Persist (R8: omit status → table default 'complete'; source='file_handler'
    //    — column added by migration 066; mirrors file_handler_worker / handler_jobs).
    if let Err(e) = sqlx::query(
        "INSERT INTO messages (session_id, agent_id, role, content, is_mirror, source) \
         VALUES ($1, $2, 'assistant', $3, true, 'file_handler')",
    )
    .bind(job.session_id)
    .bind(&job.agent_name)
    .bind(&content)
    .execute(&state.infra.db)
    .await
    {
        tracing::error!(error = %e, job_id = %job.id, "deliver_async_outcome: persist failed");
    }
}

/// Persist a chat message announcing that an async handler job FAILED, so the
/// failure is visible in the conversation (and survives reload) instead of being
/// a transient WS event the user may miss. The reason is handler/source-derived
/// (e.g. a yt-dlp / YouTube anti-bot error) — provenance-wrapped with the same
/// untrusted posture as the success path so it can't inject into the next LLM
/// turn — and length-capped. Mirrors `deliver_async_outcome`'s persist step; the
/// terminal `file_job_progress` WS event (emitted by the caller afterwards) makes
/// the UI refetch and render this row.
/// Fail a stuck 'processing' job and notify the chat + UI — the full terminal
/// failure path (transition → persist chat message → terminal WS event), shared
/// by the `/complete` callback and the worker's runtime stale-sweep (F014). Only
/// delivers if the row actually transitioned (idempotent against a racing
/// `/complete`). Returns whether it transitioned.
pub(crate) async fn fail_stuck_job_and_notify(
    state: &AppState,
    job: &handler_jobs::HandlerJob,
    reason: &str,
) -> bool {
    let transitioned = handler_jobs::mark_handler_job_failed(&state.infra.db, job.id, reason)
        .await
        .unwrap_or(false);
    if !transitioned {
        return false;
    }
    deliver_async_failure(state, job, reason).await;
    let ev = file_job_progress_event(
        &job.id.to_string(),
        &job.handler_id,
        &job.session_id.to_string(),
        "done",
        100,
        "failed",
    );
    let _ = state.channels.ui_event_tx.send(ev.to_json());
    true
}

async fn deliver_async_failure(state: &AppState, job: &handler_jobs::HandlerJob, reason: &str) {
    let upload_id = job.upload_id.map(|u| u.to_string()).unwrap_or_default();
    // Cap on char boundaries (yt-dlp errors can be long / contain multi-byte).
    let reason_capped: String = reason.chars().take(600).collect();
    let wrapped =
        crate::agent::provenance::wrap_file_output(&job.handler_id, &upload_id, &reason_capped);
    let content = format!("⚠️ Обработка не удалась ({}).\n{}", job.handler_id, wrapped);
    if let Err(e) = sqlx::query(
        "INSERT INTO messages (session_id, agent_id, role, content, is_mirror, source) \
         VALUES ($1, $2, 'assistant', $3, true, 'file_handler')",
    )
    .bind(job.session_id)
    .bind(&job.agent_name)
    .bind(&content)
    .execute(&state.infra.db)
    .await
    {
        tracing::error!(error = %e, job_id = %job.id, "deliver_async_failure: persist failed");
    }
}

/// Resolve the note target directory for `run_post_action`.
///
/// - `dir` non-empty + absolute → used as-is (operator-configured full path,
///   e.g. `/home/user/Notes`).
/// - `dir` non-empty + relative → joined under `workspace_root`.
/// - `dir` empty → `<workspace_root>/zettelkasten/<subfolder>` (historical vault
///   default), with `subfolder` validated as a single safe path component.
///
/// Returns `None` when the result would be unsafe: any `..` component in `dir`,
/// or an unsafe `subfolder`. The `filename` is validated separately by the caller.
fn resolve_note_dir(
    workspace_root: &std::path::Path,
    dir: &str,
    subfolder: &str,
) -> Option<std::path::PathBuf> {
    let dir = dir.trim();
    if !dir.is_empty() {
        let p = std::path::Path::new(dir);
        // Footgun guard: reject parent-dir traversal even for operator paths.
        if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return None;
        }
        return Some(if p.is_absolute() {
            p.to_path_buf()
        } else {
            workspace_root.join(p)
        });
    }
    if !is_safe_path_component(subfolder) {
        return None;
    }
    Some(workspace_root.join("zettelkasten").join(subfolder))
}

/// Run the optional `post_action` carried in the outcome JSON. Supports a direct
/// note write (`kind == "write_file"`, plus the legacy `"obsidian_note"` alias).
///
/// The note is written STRAIGHT TO THE FILESYSTEM — there is NO dependency on the
/// mcp-obsidian server, so the handler is self-contained/independent. The target
/// directory is operator-configured: `post_action.dir` (the `output_dir` valve,
/// a full absolute path) wins; when empty it falls back to
/// `<workspace>/zettelkasten/<subfolder>` (the historical default location).
/// mcp-obsidian remains available for agents to call directly as a tool; only
/// this auto note-write is decoupled from it.
///
/// FIX 3: uses the in-hand `outcome` directly (no DB re-read) so post_action is
/// not silently skipped when mark_handler_job_done failed.
/// Security: `filename` is validated as a single safe path component; `dir` is
/// rejected if it contains a `..` component (see `resolve_note_dir`).
///
/// Returns the workspace-relative path of the written note (for surfacing in the
/// success message), or `None` when no note was written (no post_action, unsafe
/// path, or a filesystem error).
async fn run_post_action(job_id: uuid::Uuid, outcome: &ScenarioOutcome) -> Option<String> {
    let outcome_value = serde_json::to_value(outcome).unwrap_or_else(|_| serde_json::json!({}));
    let action = match outcome_value.get("post_action") {
        Some(a) if !a.is_null() => a.clone(),
        _ => return None,
    };
    let kind = action.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    if kind != "write_file" && kind != "obsidian_note" {
        return None;
    }

    let filename = action.get("filename").and_then(|v| v.as_str()).unwrap_or("note.md");
    if !is_safe_path_component(filename) {
        tracing::warn!(
            job_id = %job_id, filename = %filename,
            "run_post_action: filename failed allowlist — skipping write"
        );
        return None;
    }
    let content = action
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or(&outcome.summary_text);

    // Operator-configured full path (`output_dir` valve) or the vault subfolder
    // (legacy `folder` field is accepted as an alias of `subfolder`).
    let dir = action.get("dir").and_then(|v| v.as_str()).unwrap_or("");
    let subfolder = action
        .get("subfolder")
        .or_else(|| action.get("folder"))
        .and_then(|v| v.as_str())
        .unwrap_or("Summary");

    let workspace_root = tokio::fs::canonicalize(crate::config::WORKSPACE_DIR)
        .await
        .unwrap_or_else(|_| std::path::PathBuf::from(crate::config::WORKSPACE_DIR));
    let target_dir = match resolve_note_dir(&workspace_root, dir, subfolder) {
        Some(d) => d,
        None => {
            tracing::warn!(
                job_id = %job_id, dir = %dir, subfolder = %subfolder,
                "run_post_action: unsafe note directory — skipping write"
            );
            return None;
        }
    };

    if let Err(e) = tokio::fs::create_dir_all(&target_dir).await {
        tracing::warn!(error = %e, job_id = %job_id, dir = %target_dir.display(), "run_post_action: create_dir_all failed");
        return None;
    }
    let path = target_dir.join(filename);
    match tokio::fs::write(&path, content).await {
        Ok(()) => {
            tracing::info!(job_id = %job_id, path = %path.display(), "run_post_action: note written");
            // Prefer a workspace-relative display path ("zettelkasten/Summary/x.md");
            // fall back to the absolute path for operator-configured out-of-workspace dirs.
            let display = path
                .strip_prefix(&workspace_root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| path.clone());
            Some(display.to_string_lossy().replace('\\', "/"))
        }
        Err(e) => {
            tracing::warn!(error = %e, job_id = %job_id, path = %path.display(), "run_post_action: write failed");
            None
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── merge_job_params tests (Phase 2d Task 3) ──────────────────────────────

    #[test]
    fn merge_job_params_merges_extra_object_fields() {
        let got = merge_job_params("ru", &json!({ "summary_length": "long" }));
        assert_eq!(got, json!({ "language": "ru", "summary_length": "long" }));
    }

    #[test]
    fn merge_job_params_empty_extra_yields_language_only() {
        let got = merge_job_params("ru", &json!({}));
        assert_eq!(got, json!({ "language": "ru" }));
    }

    #[test]
    fn merge_job_params_non_object_extra_is_ignored() {
        // Defensive: a non-object extra_params (e.g. accidental scalar) must not
        // panic or corrupt the base params — it contributes nothing.
        let got = merge_job_params("ru", &json!("not-an-object"));
        assert_eq!(got, json!({ "language": "ru" }));
    }

    // ── Async callback helper tests (Task 3) ──────────────────────────────────

    #[test]
    fn file_job_progress_event_has_generic_shape() {
        let ev: serde_json::Value = serde_json::from_str(
            &file_job_progress_event(
                "job-1",
                "summarize_video",
                "sess-9",
                "digest",
                42,
                "processing",
            )
            .to_json(),
        )
        .unwrap();
        assert_eq!(ev["type"], "file_job_progress");
        assert_eq!(ev["job_id"], "job-1");
        assert_eq!(ev["handler_id"], "summarize_video");
        assert_eq!(ev["session_id"], "sess-9");
        assert_eq!(ev["phase"], "digest");
        assert_eq!(ev["pct"], 42);
        assert_eq!(ev["status"], "processing");
    }

    #[test]
    fn parse_outcome_four_key_json_defaults_video_accepted() {
        // R9: toolgate emits 4 keys; ScenarioOutcome has a 5th (video_accepted,
        // serde default) — deserialization must succeed with it false.
        let raw = r#"{"status":"ok","summary_text":"привет мир","artifact_urls":["/api/uploads/1?sig=x"],"reason":null}"#;
        let o: crate::agent::file_scenario::outcome::ScenarioOutcome =
            serde_json::from_str(raw).unwrap();
        assert_eq!(o.status, crate::agent::file_scenario::outcome::ScenarioStatus::Ok);
        assert_eq!(o.summary_text, "привет мир");
        assert_eq!(o.artifact_urls, vec!["/api/uploads/1?sig=x".to_string()]);
        assert!(!o.video_accepted, "missing key defaults to false");
    }

    #[test]
    fn parse_outcome_too_large_from_toolgate_json() {
        let raw = r#"{"status":"too_large","summary_text":"","artifact_urls":[],"reason":"over 50MB"}"#;
        let o: crate::agent::file_scenario::outcome::ScenarioOutcome =
            serde_json::from_str(raw).unwrap();
        assert_eq!(
            o.status,
            crate::agent::file_scenario::outcome::ScenarioStatus::TooLarge
        );
        assert_eq!(o.reason.as_deref(), Some("over 50MB"));
    }

    #[test]
    fn persisted_content_carries_file_output_wrapper() {
        // The persist body for an ok outcome is the wrapped summary (R4).
        let upload = "11111111-1111-1111-1111-111111111111";
        let wrapped =
            crate::agent::provenance::wrap_file_output("transcribe", upload, "распознанный текст");
        assert!(wrapped.starts_with(&format!(
            "<file_output handler=\"transcribe\" upload=\"{upload}\" trust=\"untrusted\">"
        )));
        assert!(wrapped.contains("\nраспознанный текст\n"));
    }

    // ── Job-callback token gate tests (non-DB, unit) ─────────────────────────

    #[tokio::test]
    async fn run_post_action_writes_note_to_absolute_dir() {
        // Full E2E of the write path (no LLM/MCP): a write_file post_action with
        // an absolute `dir` must produce the note file at that exact path.
        let base = std::env::temp_dir().join(format!("opex_valve_{}", uuid::Uuid::new_v4()));
        let target = base.join("Конспекты");
        let outcome = ScenarioOutcome {
            status: ScenarioStatus::Ok,
            summary_text: "short".to_string(),
            artifact_urls: vec![],
            reason: None,
            video_accepted: false,
            post_action: Some(serde_json::json!({
                "kind": "write_file",
                "dir": target.to_string_lossy(),
                "subfolder": "Summary",
                "filename": "note.md",
                "content": "hello from the output_dir valve",
            })),
        };

        let saved = run_post_action(uuid::Uuid::new_v4(), &outcome).await;

        let written = target.join("note.md");
        let body = tokio::fs::read_to_string(&written).await;
        let _ = tokio::fs::remove_dir_all(&base).await;
        assert_eq!(body.unwrap(), "hello from the output_dir valve");
        // New contract: on a successful write, the note's display path is returned
        // (absolute here, since `target` is outside the workspace) for the success
        // message. Path is normalised to forward slashes.
        let saved = saved.expect("run_post_action returns the written path");
        assert!(saved.ends_with("Конспекты/note.md"), "unexpected saved path: {saved}");
    }

    #[tokio::test]
    async fn run_post_action_rejects_traversal_filename() {
        // A filename that is not a safe component must NOT be written.
        let base = std::env::temp_dir().join(format!("opex_valve_{}", uuid::Uuid::new_v4()));
        let outcome = ScenarioOutcome {
            status: ScenarioStatus::Ok,
            summary_text: String::new(),
            artifact_urls: vec![],
            reason: None,
            video_accepted: false,
            post_action: Some(serde_json::json!({
                "kind": "write_file",
                "dir": base.to_string_lossy(),
                "filename": "../escape.md",
                "content": "nope",
            })),
        };
        run_post_action(uuid::Uuid::new_v4(), &outcome).await;
        // Nothing created (bad filename → skipped before create_dir_all).
        assert!(!base.exists());
    }

    #[test]
    fn resolve_note_dir_cases() {
        use std::path::{Path, PathBuf};
        let ws = Path::new("/ws");
        // Empty dir → vault/<subfolder> (historical default).
        assert_eq!(
            resolve_note_dir(ws, "", "Summary"),
            Some(PathBuf::from("/ws/zettelkasten/Summary"))
        );
        assert_eq!(
            resolve_note_dir(ws, "   ", "Videos"),
            Some(PathBuf::from("/ws/zettelkasten/Videos"))
        );
        // Absolute operator path used verbatim (full-path valve).
        assert_eq!(
            resolve_note_dir(ws, "/home/u/Notes", "Summary"),
            Some(PathBuf::from("/home/u/Notes"))
        );
        // Relative dir joined under the workspace root.
        assert_eq!(
            resolve_note_dir(ws, "Custom/Notes", "Summary"),
            Some(PathBuf::from("/ws/Custom/Notes"))
        );
        // Traversal rejected in dir and in subfolder.
        assert_eq!(resolve_note_dir(ws, "/home/../etc", "Summary"), None);
        assert_eq!(resolve_note_dir(ws, "a/../b", "Summary"), None);
        assert_eq!(resolve_note_dir(ws, "", "../evil"), None);
        assert_eq!(resolve_note_dir(ws, "", "a/b"), None);
    }

    #[test]
    fn verify_job_callback_token_accepts_valid() {
        let key = [1u8; 32];
        let id = uuid::Uuid::new_v4();
        let token = crate::uploads::mint_job_callback_token(&key, id, 300);
        assert!(crate::uploads::verify_job_callback_token(&key, id, &token));
    }

    #[test]
    fn verify_job_callback_token_rejects_missing() {
        let key = [2u8; 32];
        let id = uuid::Uuid::new_v4();
        assert!(!crate::uploads::verify_job_callback_token(&key, id, ""));
    }

    #[test]
    fn verify_job_callback_token_rejects_wrong_job_id() {
        let key = [3u8; 32];
        let id1 = uuid::Uuid::new_v4();
        let id2 = uuid::Uuid::new_v4();
        let token = crate::uploads::mint_job_callback_token(&key, id1, 300);
        assert!(!crate::uploads::verify_job_callback_token(&key, id2, &token));
    }

    #[test]
    fn verify_job_callback_token_rejects_tampered() {
        let key = [4u8; 32];
        let id = uuid::Uuid::new_v4();
        let token = crate::uploads::mint_job_callback_token(&key, id, 300);
        let tampered = format!("{}.{}", token.split('.').next().unwrap(), "00".repeat(32));
        assert!(!crate::uploads::verify_job_callback_token(&key, id, &tampered));
    }

    // ── post_action path-traversal allowlist tests ────────────────────────────

    #[test]
    fn path_component_allowlist_accepts_valid_names() {
        assert!(is_safe_path_component("Summary"));
        assert!(is_safe_path_component("note.md"));
        assert!(is_safe_path_component("My Notes 2024"));
        assert!(is_safe_path_component("file_name-v2.txt"));
    }

    #[test]
    fn path_component_allowlist_rejects_traversal() {
        assert!(!is_safe_path_component("../etc/passwd"));
        assert!(!is_safe_path_component("a/b"));
        assert!(!is_safe_path_component("a\\b"));
        assert!(!is_safe_path_component(".."));
        assert!(!is_safe_path_component(""));
    }

    #[test]
    fn path_component_allowlist_rejects_too_long() {
        let long = "a".repeat(129);
        assert!(!is_safe_path_component(&long));
    }

    // ── DB-backed tests (require DATABASE_URL — skipped without it) ───────────
    // Run with `make test-db` or with DATABASE_URL set.

    #[sqlx::test(migrations = "../../migrations")]
    async fn owner_gate_accepts_client_upload_and_yields_mime(pool: sqlx::PgPool) {
        // `insert_with_retention(pool, owner_type, owner_id: Option<&str>, mime, data: &[u8], retention_days)`.
        // Pass a byte slice (`b"OggSfake"` coerces to `&[u8]`) — the `data` param is `&[u8]`,
        // an owned `Vec<u8>` does NOT auto-coerce there.
        let id = crate::db::uploads::insert_with_retention(
            &pool,
            "client_upload",
            Some("user-1"),
            "audio/ogg",
            b"OggSfake",
            30,
        )
        .await
        .unwrap();
        let meta = assert_upload_accessible(&pool, id).await.unwrap();
        assert_eq!(meta.mime, "audio/ogg");
        assert_eq!(meta.size, b"OggSfake".len() as u64); // 8

        // missing upload → 404
        let err = assert_upload_accessible(&pool, uuid::Uuid::new_v4())
            .await
            .unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::NOT_FOUND);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn messages_source_column_exists_after_066(pool: sqlx::PgPool) {
        // Migration 066 must apply: inserting with source='file_handler' succeeds
        // and the column defaults NULL otherwise. session_id is left NULL to avoid
        // the messages_session_id_fkey constraint (this test only exercises the new
        // `source` column, not the session relation).
        sqlx::query(
            r#"INSERT INTO messages (session_id, agent_id, role, content, source)
               VALUES (NULL, 'Atlas', 'assistant', 'x', 'file_handler')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let src: Option<String> = sqlx::query_scalar(
            r#"SELECT source FROM messages WHERE source = 'file_handler' LIMIT 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(src.as_deref(), Some("file_handler"));
    }
}

// ── Async-enqueue seam tests (Task 4, Phase 5) ───────────────────────────────

#[cfg(test)]
mod async_enqueue_tests {

}
