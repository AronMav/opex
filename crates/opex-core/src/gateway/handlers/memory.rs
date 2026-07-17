use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use opex_db::memory_queries;
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::InfraServices;
use crate::memory::EmbeddingService;

include!("memory_dto_structs.rs");

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/memory/stats", get(api_memory_stats))
        .route("/api/memory/reindex", post(api_reindex_memory))
        .route("/api/memory/fts-language", get(api_get_fts_language).put(api_set_fts_language))
        .route("/api/memory/documents", get(api_list_documents))
        .route("/api/memory/documents/{id}", get(api_get_document).patch(api_patch_document).delete(api_delete_memory))
}

// ── Memory API ──

pub(crate) async fn api_memory_stats(State(state): State<InfraServices>) -> Json<MemoryStatsDto> {
    // Post-m033: every chunk is its own document — `total` and `documents` are equal.
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks")
        .fetch_one(&state.db).await
        .inspect_err(|e| tracing::error!(error = %e, "stats: failed to count chunks"))
        .unwrap_or(0);

    let pinned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks WHERE pinned = true")
        .fetch_one(&state.db).await
        .inspect_err(|e| tracing::error!(error = %e, "stats: failed to count pinned"))
        .unwrap_or(0);

    let avg_score: f64 = sqlx::query_scalar("SELECT COALESCE(AVG(relevance_score), 0) FROM memory_chunks")
        .fetch_one(&state.db).await
        .inspect_err(|e| tracing::error!(error = %e, "stats: failed to get avg score"))
        .unwrap_or(0.0);

    let (t_pending, t_processing, t_done, t_failed) = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "SELECT
            COUNT(*) FILTER (WHERE status = 'pending'),
            COUNT(*) FILTER (WHERE status = 'processing'),
            COUNT(*) FILTER (WHERE status = 'done'),
            COUNT(*) FILTER (WHERE status = 'failed')
         FROM memory_tasks"
    ).fetch_one(&state.db).await.unwrap_or((0, 0, 0, 0));

    let embed_dim = state.embedder.embed_dim();
    let embed_model = state.embedder.embed_provider_display().unwrap_or_default();

    Json(MemoryStatsDto {
        total,
        total_chunks: total,
        pinned,
        avg_score,
        embed_model: if embed_model.is_empty() { None } else { Some(embed_model) },
        embed_dim: if embed_dim > 0 { Some(embed_dim as i32) } else { None },
        tasks: MemoryTaskStatsDto {
            pending: t_pending,
            processing: t_processing,
            done: t_done,
            failed: t_failed,
        },
    })
}



#[derive(Debug, sqlx::FromRow)]
struct DocumentRow {
    id: uuid::Uuid,
    source: Option<String>,
    pinned: bool,
    relevance_score: f64,
    created_at: chrono::DateTime<chrono::Utc>,
    accessed_at: chrono::DateTime<chrono::Utc>,
    preview: Option<String>,
    total_chars: Option<i64>,
    #[sqlx(default)]
    scope: String,
    kind: String,
    importance: f32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DocumentsQuery {
    query: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(crate) async fn api_list_documents(
    State(state): State<InfraServices>,
    Query(q): Query<DocumentsQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(20).min(100);
    let offset = q.offset.unwrap_or(0).max(0);

    // Search mode: search at chunk level, group by document
    if let Some(ref search) = q.query
        && !search.trim().is_empty() {
            return match state.memory_store.search(search, (limit * 5) as usize, &[], "").await {
                Ok((results, mode)) => {
                    // Post-m033: every chunk is its own document (no parent grouping).
                    let total_found = results.len() as i64;
                    let page: Vec<_> = results.iter().skip(offset as usize).take(limit as usize).collect();

                    let documents: Vec<MemoryDocumentDto> = page.iter().map(|r| MemoryDocumentDto {
                        id: r.id.clone(),
                        source: Some(r.source.clone()),
                        pinned: r.pinned,
                        relevance_score: r.relevance_score,
                        similarity: Some(r.similarity),
                        created_at: None,
                        accessed_at: None,
                        preview: Some(r.content.chars().take(200).collect()),
                        total_chars: Some(r.content.len() as i64),
                        scope: None,
                        // Search results come from generic search, which is kind-filtered to
                        // 'fact' (soul events/reflections are excluded), so hardcoding these
                        // is correct by construction. If that upstream filter is ever loosened,
                        // MemoryResult must carry kind/importance instead.
                        kind: "fact".to_string(),
                        importance: 5.0,
                    }).collect();
                    Json(json!({ "documents": documents, "total": total_found, "search_mode": mode })).into_response()
                }
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
            };
        }

    // List mode: every chunk is a top-level document (post-m033).
    let sql = "SELECT \
           m.id, m.source, m.pinned, \
           COALESCE(m.relevance_score, 1.0) AS relevance_score, \
           m.created_at, COALESCE(m.accessed_at, m.created_at) AS accessed_at, \
           LEFT(m.content, 200) AS preview, \
           LENGTH(m.content)::bigint AS total_chars, \
           m.scope, m.kind, m.importance \
         FROM memory_chunks m \
         ORDER BY COALESCE(m.accessed_at, m.created_at) DESC \
         LIMIT $1 OFFSET $2";

    let rows = sqlx::query_as::<_, DocumentRow>(sql)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await;

    match rows {
        Ok(rows) => {
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks")
                .fetch_one(&state.db).await.unwrap_or(0);
            let documents: Vec<MemoryDocumentDto> = rows.iter().map(|r| MemoryDocumentDto {
                id: r.id.to_string(),
                source: r.source.clone(),
                pinned: r.pinned,
                relevance_score: r.relevance_score,
                similarity: None,
                created_at: Some(r.created_at.to_rfc3339()),
                accessed_at: Some(r.accessed_at.to_rfc3339()),
                preview: r.preview.clone(),
                total_chars: r.total_chars,
                scope: if r.scope.is_empty() { None } else { Some(r.scope.clone()) },
                kind: r.kind.clone(),
                importance: r.importance,
            }).collect();
            Json(json!({ "documents": documents, "total": total })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_get_document(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    // Post-m033: every chunk is its own document — single row.
    let row = sqlx::query_as::<_, (String, Option<String>, bool, f64, chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>(
        "SELECT content, source, pinned, COALESCE(relevance_score,1.0), created_at, COALESCE(accessed_at,created_at) \
         FROM memory_chunks WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    match row {
        Ok(Some((content, source, pinned, score, created, accessed))) => {
            let total_chars = content.len();
            Json(json!({
                "id": id,
                "source": source,
                "pinned": pinned,
                "relevance_score": score,
                "created_at": created.to_rfc3339(),
                "accessed_at": accessed.to_rfc3339(),
                "content": content,
                "total_chars": total_chars,
            })).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "document not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// Fail-closed biography guard for the UI-facing delete/patch routes (spec §5.2).
/// Returns `Some(refusal)` when the chunk is a soul biography row (`kind != 'fact'`)
/// OR when its kind cannot be verified (DB error) — never let a mutation through on
/// an unverifiable kind. Returns `None` only when it is safe to proceed (`kind = 'fact'`,
/// or the row is absent — the caller's own `rows_affected` check reports NOT_FOUND).
/// Deliberate biography removal uses the raw-SQL quarantine runbook, not these endpoints.
async fn refuse_if_biography(
    db: &sqlx::PgPool,
    id: uuid::Uuid,
) -> Option<axum::response::Response> {
    let kind: Option<String> = match sqlx::query_scalar("SELECT kind FROM memory_chunks WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
    {
        Ok(k) => k,
        Err(e) => {
            return Some(
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("cannot verify chunk kind: {e}")})),
                )
                    .into_response(),
            );
        }
    };
    if matches!(kind.as_deref(), Some(k) if k != "fact") {
        return Some(
            (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "biography chunks (event/reflection) are immutable via this endpoint"})),
            )
                .into_response(),
        );
    }
    None
}

pub(crate) async fn api_patch_document(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<PatchMemoryRequest>,
) -> impl IntoResponse {
    // Fail-closed: soul biography rows are immutable via the UI patch route.
    if let Some(refusal) = refuse_if_biography(&state.db, id).await {
        return refusal;
    }
    if let Some(pinned) = req.pinned {
        let result = sqlx::query("UPDATE memory_chunks SET pinned = $2 WHERE id = $1")
            .bind(id).bind(pinned).execute(&state.db).await;
        match result {
            Ok(r) if r.rows_affected() > 0 => {}
            Ok(_) => return (StatusCode::NOT_FOUND, Json(json!({"error": "document not found"}))).into_response(),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
        }
    }
    Json(json!({"ok": true})).into_response()
}

pub(crate) async fn api_delete_memory(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    // Fail-closed: soul biography rows cannot be destroyed via the UI delete route
    // (spec §5.2, revised — operator quarantine goes through the raw-SQL runbook).
    if let Some(refusal) = refuse_if_biography(&state.db, id).await {
        return refusal;
    }
    let result = sqlx::query("DELETE FROM memory_chunks WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => {
            crate::db::audit::audit_spawn(state.db.clone(), String::new(), crate::db::audit::event_types::MEMORY_DELETED, None, json!({"chunk_id": id.to_string()}));
            Json(json!({"ok": true})).into_response()
        }
        Ok(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "chunk not found"}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PatchMemoryRequest {
    pinned: Option<bool>,
}

// ── FTS Language API ──

/// GET /api/memory/fts-language — return current FTS language and available options.
pub(crate) async fn api_get_fts_language(State(state): State<InfraServices>) -> Json<Value> {
    let current = state.memory_store.fts_language();
    Json(json!({
        "language": current,
        "available": [
            "simple", "danish", "dutch", "english", "finnish", "french",
            "german", "hungarian", "italian", "norwegian", "portuguese",
            "romanian", "russian", "spanish", "swedish", "turkish"
        ]
    }))
}

/// PUT /api/memory/fts-language — change FTS language and rebuild tsvector index.
pub(crate) async fn api_set_fts_language(
    State(state): State<InfraServices>,
    Json(req): Json<Value>,
) -> impl IntoResponse {
    let lang = match req.get("language").and_then(|v| v.as_str()) {
        Some(l) => l.to_string(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "'language' is required"}))).into_response(),
    };

    // Validate
    let valid = [
        "simple", "danish", "dutch", "english", "finnish", "french",
        "german", "hungarian", "italian", "norwegian", "portuguese",
        "romanian", "russian", "spanish", "swedish", "turkish",
    ];
    if !valid.contains(&lang.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("unsupported language: {}", lang)}))).into_response();
    }

    state.memory_store.set_fts_language(&lang);

    match state.memory_store.rebuild_fts().await {
        Ok(rows) => {
            // Persist so the change survives restarts (TOML override still wins
            // if explicitly set; this writes to system_flags only).
            if let Err(e) = opex_db::sys_flags::upsert(
                &state.db,
                "memory.fts_language",
                Value::String(lang.clone()),
            )
            .await
            {
                tracing::warn!(error = %e, "FTS language updated in-memory + tsv rebuilt, but failed to persist to system_flags");
            }
            Json(json!({
                "ok": true,
                "language": lang,
                "rows_rebuilt": rows,
            })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ).into_response(),
    }
}

// ── Memory Reindex ──

/// `POST /api/memory/reindex` — drop shared-scope `memory_chunks` (workspace
/// files), rebuild the HNSW index for the current embedding dimension, enqueue
/// a single bulk reindex task that the memory worker will pick up, and clear
/// the `dim_mismatch` flag.
///
/// **Scope:** only `scope='shared'` chunks are deleted — agent-private and
/// session-transcript chunks are preserved. (Previous behaviour did a global
/// `TRUNCATE memory_chunks`, which destroyed all private knowledge.)
///
/// **Tasks:** a single bulk `reindex` task is enqueued — the worker handler
/// at `crates/opex-memory-worker/src/handlers/reindex.rs` walks every
/// indexable workspace file inside one job. (Previous behaviour enqueued
/// N per-file tasks whose `{"source": ...}` payload the worker ignored, so
/// each task re-walked the full workspace — O(N²) embeddings.)
///
/// Protected by `infra.reindex_mutex` — a second concurrent caller gets `409
/// Conflict`. The mutex is held for the full duration of the destructive +
/// rebuild sequence so a reset cannot interleave with task enqueue.
pub(crate) async fn api_reindex_memory(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    let _guard = match infra.reindex_mutex.try_lock() {
        Ok(g) => g,
        Err(_) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "reindex already in progress"})),
            )
                .into_response();
        }
    };

    match run_reindex(&infra.db, infra.embedder.clone()).await {
        Ok((sources_to_index, previous_dim, new_dim)) => Json(json!({
            "task_enqueued": true,
            "sources_to_index": sources_to_index,
            "previous_dim": previous_dim,
            "new_dim": new_dim,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn run_reindex(
    db: &sqlx::PgPool,
    embedder: Arc<dyn EmbeddingService>,
) -> anyhow::Result<(usize, Option<u32>, u32)> {
    // 1. Sync embedder state (loads persistent dim-mismatch flag + probes
    //    embedding dimension if not yet detected). This is a single probe
    //    via `client.probe_dim()` — no retry-policy bait that could block
    //    this HTTP request (and the `reindex_mutex`) for up to 180s like
    //    the old `embed("probe")` fallback did.
    embedder.ensure_initialized().await;
    let new_dim = embedder.embed_dim();
    if new_dim == 0 {
        anyhow::bail!(
            "embedder not initialized, cannot reindex (check toolgate/embedding provider)"
        );
    }

    let previous_dim = memory_queries::get_existing_embedding_dim(db)
        .await
        .map(|d| d as u32);

    // 2. Drop SHARED-scope storage only — workspace files are the only thing
    //    the worker will rebuild. Private (agent-scoped) and session chunks
    //    are preserved. Rebuild the HNSW index at the new dimension.
    sqlx::query("DELETE FROM memory_chunks WHERE scope = 'shared'")
        .execute(db)
        .await?;
    memory_queries::drop_hnsw_index(db).await?;
    memory_queries::ensure_hnsw_index(db, new_dim).await?;

    // 3. Enumerate workspace sources (for response feedback only — worker
    //    re-walks the workspace itself inside its bulk handler).
    let workspace_sources = crate::agent::workspace::list_indexable_files()?;
    let sources_to_index = workspace_sources.len();

    // 4. Enqueue a SINGLE bulk reindex task. The worker handler reads
    //    `clear_existing`, `include_sessions`, `agent_id` — `source` is
    //    ignored. We set `clear_existing=false` because we already dropped
    //    shared chunks above; `include_sessions=false` because per-agent
    //    session indexing belongs to a separate flow.
    memory_queries::enqueue_reindex_task(
        db,
        json!({
            "clear_existing": false,
            "include_sessions": false,
            "agent_id": "",
        }),
    )
    .await?;

    // 5. Clear the dim_mismatch flag (in-memory + persistent system_flags).
    embedder.clear_dim_mismatch().await?;

    Ok((sources_to_index, previous_dim, new_dim))
}

