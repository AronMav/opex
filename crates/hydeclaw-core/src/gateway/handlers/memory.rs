use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, delete},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::InfraServices;

include!("memory_dto_structs.rs");

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/memory", get(api_list_memory).post(api_create_memory))
        .route("/api/memory/stats", get(api_memory_stats))
        .route("/api/memory/export", get(api_export_memory))
        .route("/api/memory/fts-language", get(api_get_fts_language).put(api_set_fts_language))
        .route("/api/memory/{id}", delete(api_delete_memory).patch(api_patch_memory))
        .route("/api/memory/tasks", get(api_memory_tasks))
        .route("/api/memory/documents", get(api_list_documents))
        .route("/api/memory/documents/{id}", get(api_get_document).patch(api_patch_document).delete(api_delete_memory))
}

// ── Memory API ──

#[derive(Debug, Deserialize)]
pub(crate) struct MemoryQuery {
    query: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(crate) async fn api_list_memory(
    State(state): State<InfraServices>,
    Query(q): Query<MemoryQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(20).min(100) as usize;
    let offset = q.offset.unwrap_or(0).max(0);

    // Search with query: semantic → FTS fallback (handled inside MemoryStore::search)
    if let Some(ref search) = q.query
        && !search.trim().is_empty() {
            match state.memory_store.search(search, limit, &[], "").await {
                Ok((results, mode)) => {
                    let chunks: Vec<Value> = results
                        .iter()
                        .map(|r| {
                            json!({
                                "id": r.id,
                                "content": r.content,
                                "source": r.source,
                                "relevance_score": r.relevance_score,
                                "similarity": r.similarity,
                                "pinned": r.pinned,
                            })
                        })
                        .collect();
                    return Json(json!({ "chunks": chunks, "search_mode": mode })).into_response();
                }
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": e.to_string()})),
                    ).into_response();
                }
            }
        }

    // Admin endpoint: shows all chunks (no agent_id filter). Protected by auth middleware.
    // No query: list all chunks by relevance
    let result = sqlx::query_as::<_, MemoryChunkRow>(
        "SELECT id, content, source, relevance_score, pinned, created_at, accessed_at, scope, agent_id \
         FROM memory_chunks ORDER BY relevance_score DESC LIMIT $1 OFFSET $2",
    )
    .bind(limit as i64)
    .bind(offset)
    .fetch_all(&state.db)
    .await;

    match result {
        Ok(rows) => {
            let chunks: Vec<Value> = rows
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "content": c.content,
                        "source": c.source,
                        "relevance_score": c.relevance_score,
                        "pinned": c.pinned,
                        "created_at": c.created_at.to_rfc3339(),
                        "accessed_at": c.accessed_at.to_rfc3339(),
                        "scope": c.scope,
                        "agent_id": c.agent_id,
                    })
                })
                .collect();
            Json(json!({ "chunks": chunks })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct MemoryChunkRow {
    id: uuid::Uuid,
    content: String,
    source: Option<String>,
    relevance_score: f64,
    pinned: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    accessed_at: chrono::DateTime<chrono::Utc>,
    #[sqlx(default)]
    scope: String,
    #[sqlx(default)]
    agent_id: String,
}

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
    let embed_model = state.embedder.embed_model_name().unwrap_or_default();

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


/// GET /api/memory/tasks — list memory worker tasks
pub(crate) async fn api_memory_tasks(State(state): State<InfraServices>) -> Json<Value> {
    let rows = sqlx::query_as::<_, (uuid::Uuid, String, String, serde_json::Value, Option<String>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, task_type, status, params, error, created_at FROM memory_tasks ORDER BY created_at DESC LIMIT 50"
    ).fetch_all(&state.db).await.unwrap_or_default();
    let tasks: Vec<Value> = rows.iter().map(|(id, tt, st, p, e, ca)| json!({
        "id": id, "task_type": tt, "status": st, "params": p, "error": e, "created_at": ca.to_rfc3339()
    })).collect();
    Json(json!({"tasks": tasks}))
}

// ── Documents API (document-level view) ──

#[derive(Debug, sqlx::FromRow)]
struct DocumentRow {
    id: uuid::Uuid,
    source: Option<String>,
    pinned: bool,
    relevance_score: f64,
    created_at: chrono::DateTime<chrono::Utc>,
    accessed_at: chrono::DateTime<chrono::Utc>,
    preview: Option<String>,
    chunks_count: i64,
    total_chars: Option<i64>,
    #[sqlx(default)]
    scope: String,
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
                        // chunks_count is always 1 after m033; kept in DTO for shape compat.
                        chunks_count: 1,
                        total_chars: Some(r.content.len() as i64),
                        scope: None,
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
           1::bigint AS chunks_count, \
           LENGTH(m.content)::bigint AS total_chars, \
           m.scope \
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
                chunks_count: r.chunks_count,
                total_chars: r.total_chars,
                scope: if r.scope.is_empty() { None } else { Some(r.scope.clone()) },
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
                "chunks_count": 1,
                "total_chars": total_chars,
            })).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "document not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_patch_document(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<PatchMemoryRequest>,
) -> impl IntoResponse {
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

// POST /api/memory — create a new memory chunk
#[derive(Debug, Deserialize)]
pub(crate) struct CreateMemoryRequest {
    content: String,
    source: Option<String>,
    pinned: Option<bool>,
}

pub(crate) async fn api_create_memory(
    State(state): State<InfraServices>,
    Json(req): Json<CreateMemoryRequest>,
) -> impl IntoResponse {
    if req.content.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "content must not be empty"})),
        )
            .into_response();
    }
    let source = req.source.as_deref().unwrap_or("ui");
    let pinned = req.pinned.unwrap_or(false);
    // Admin-created chunks are shared so all agents can see them
    match state.memory_store.index(&req.content, source, pinned, "shared", "").await {
        Ok(id) => Json(json!({"id": id, "ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn api_delete_memory(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
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
    content: Option<String>,
}

pub(crate) async fn api_patch_memory(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<PatchMemoryRequest>,
) -> impl IntoResponse {
    if req.pinned.is_none() && req.content.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "nothing to update"})),
        )
            .into_response();
    }

    // Validate content early — before any DB writes
    if let Some(ref content) = req.content
        && content.trim().is_empty() {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "content must not be empty"}))).into_response();
        }

    // Update pinned flag if provided
    if let Some(pinned) = req.pinned {
        let result = sqlx::query("UPDATE memory_chunks SET pinned = $2 WHERE id = $1")
            .bind(id)
            .bind(pinned)
            .execute(&state.db)
            .await;
        match result {
            Ok(r) if r.rows_affected() > 0 => {
                crate::db::audit::audit_spawn(state.db.clone(), String::new(), crate::db::audit::event_types::MEMORY_PINNED, None, json!({"chunk_id": id.to_string(), "pinned": pinned}));
            }
            Ok(_) => {
                return (StatusCode::NOT_FOUND, Json(json!({"error": "chunk not found"}))).into_response();
            }
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
            }
        }
    }

    // Update content if provided — re-embed and rebuild tsvector
    if let Some(ref content) = req.content {
        let embedding = match state.embedder.embed(content).await {
            Ok(e) => e,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("embedding failed: {e}")}))).into_response();
            }
        };
        let vec_str = crate::memory::fmt_vec(&embedding);
        let lang = match state.memory_store.validated_fts_language() {
            Ok(l) => l,
            Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "invalid FTS language configuration"}))).into_response(),
        };
        // SAFETY: `lang` comes from `validated_fts_language()` which allowlists lowercase ASCII identifiers only.
        let sql = format!(
            "UPDATE memory_chunks SET content = $2, embedding = $3::halfvec, tsv = to_tsvector('{lang}', $2) WHERE id = $1"
        );
        let result = sqlx::query(&sql)
            .bind(id)
            .bind(content)
            .bind(&vec_str)
            .execute(&state.db)
            .await;
        match result {
            Ok(r) if r.rows_affected() > 0 => {}
            Ok(_) => {
                return (StatusCode::NOT_FOUND, Json(json!({"error": "chunk not found"}))).into_response();
            }
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
            }
        }
    }

    Json(json!({"ok": true})).into_response()
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
        Ok(rows) => Json(json!({
            "ok": true,
            "language": lang,
            "rows_rebuilt": rows,
        })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ).into_response(),
    }
}

// ── Memory Export ──

/// GET /api/memory/export — bulk export all memory chunks (without embeddings).
/// Limited to 100k chunks to prevent OOM.
pub(crate) async fn api_export_memory(
    State(state): State<InfraServices>,
) -> impl IntoResponse {
    const EXPORT_LIMIT: i64 = 100_000;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks")
        .fetch_one(&state.db).await.unwrap_or(0);
    if total > EXPORT_LIMIT {
        tracing::warn!(total, limit = EXPORT_LIMIT, "memory export truncated");
    }
    match sqlx::query_as::<_, (uuid::Uuid, String, Option<String>, bool, f64, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, content, source, pinned, relevance_score, created_at \
         FROM memory_chunks ORDER BY created_at LIMIT $1",
    )
    .bind(EXPORT_LIMIT)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => {
            let chunks: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "id": r.0,
                        "content": r.1,
                        "source": r.2,
                        "pinned": r.3,
                        "relevance_score": r.4,
                        "created_at": r.5.to_rfc3339(),
                    })
                })
                .collect();
            Json(json!({ "chunks": chunks, "total": chunks.len() })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

