use anyhow::{Context, Result};
use sqlx::PgPool;

use chrono::{DateTime, Utc};

/// Search result row from memory_chunks (ranked).
pub struct MemoryResult {
    pub id: String,
    pub content: String,
    pub source: String,
    pub pinned: bool,
    pub relevance_score: f64,
    pub similarity: f64,
}

/// Stored memory chunk (single row from memory_chunks).
pub struct MemoryChunk {
    pub id: String,
    pub content: String,
    pub source: String,
    pub pinned: bool,
    pub relevance_score: f64,
    pub created_at: DateTime<Utc>,
    // `accessed_at` is read by `scheduler::run_memory_decay` via raw SQL
    // (decay formula uses `now() - accessed_at`); the Rust struct copy is
    // currently unread. Kept for future use when struct-side decay runs locally.
    pub accessed_at: DateTime<Utc>,
    pub kind: String,
    pub importance: f32,
}

/// Shared INSERT SQL for `memory_chunks`. Lang is bound as `$8::regconfig` so
/// Postgres validates the dictionary name (no string interpolation, no
/// hand-rolled whitelist). Invalid configs surface as a Postgres error and are
/// mapped to a domain error by [`map_fts_lang_error`].
const INSERT_CHUNK_SQL: &str = r"
    INSERT INTO memory_chunks
        (id, agent_id, content, embedding, source, pinned, relevance_score, tsv, scope, kind, importance, lineage, valence)
    VALUES
        ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector($8::regconfig, $3), $7, $9, $10, $11, $12)
";

/// Translate Postgres errors raised by an unknown text-search config
/// (`$N::regconfig` cast or `to_tsvector` lookup) into a clear domain error
/// that names the offending input. Other DB errors pass through as-is.
///
/// Two SQLSTATEs surface in practice:
/// - `42704` (`undefined_object`) — `'klingon'::regconfig` cast failure
/// - `22023` (`invalid_parameter_value`) — alternative path for some configs
fn map_fts_lang_error(lang: &str, e: sqlx::Error) -> anyhow::Error {
    if let Some(db_err) = e.as_database_error()
        && matches!(db_err.code().as_deref(), Some("42704") | Some("22023"))
    {
        return anyhow::anyhow!(
            "invalid FTS language: '{lang}' (must match a Postgres text search config)"
        );
    }
    anyhow::Error::new(e)
}

// ── Helper ───────────────────────────────────────────────────────────────────

/// Map a sqlx Row to `MemoryResult`.
fn row_to_memory_result(r: &sqlx::postgres::PgRow) -> MemoryResult {
    use sqlx::Row;
    MemoryResult {
        id: r.get("id"),
        content: r.get("content"),
        source: r.get("source"),
        pinned: r.get("pinned"),
        relevance_score: r.get("relevance_score"),
        similarity: r.get("similarity"),
    }
}

/// Map a sqlx Row to `MemoryChunk`.
fn row_to_memory_chunk(r: &sqlx::postgres::PgRow) -> MemoryChunk {
    use sqlx::Row;
    MemoryChunk {
        id: r.get("id"),
        content: r.get("content"),
        source: r.get("source"),
        pinned: r.get("pinned"),
        relevance_score: r.get("relevance_score"),
        created_at: r.get("created_at"),
        accessed_at: r.get("accessed_at"),
        kind: r.get("kind"),
        importance: r.get::<f32, _>("importance"),
    }
}

// ── Initialize ───────────────────────────────────────────────────────────────

/// Check the dimension of existing embeddings in the database.
pub async fn get_existing_embedding_dim(db: &PgPool) -> Option<i32> {
    sqlx::query_scalar(
        "SELECT vector_dims(embedding)::int FROM memory_chunks WHERE embedding IS NOT NULL LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .unwrap_or(None)
}

/// Delete all memory chunks that have embeddings (dimension mismatch cleanup).
/// Soul biography (kind event/reflection) is NOT file-backed and cannot be
/// re-populated by reindex — its rows survive with embedding=NULL (unretrievable
/// by L1 until a future re-embed pass; reflection window still works — it
/// reads by created_at, no embedding needed). Spec §1 amendment (plan review).
pub async fn clear_embeddings(db: &PgPool) -> Result<()> {
    sqlx::query("UPDATE memory_chunks SET embedding = NULL WHERE embedding IS NOT NULL AND kind != 'fact'")
        .execute(db)
        .await
        .context("failed to null soul embeddings after dimension change")?;
    sqlx::query("DELETE FROM memory_chunks WHERE embedding IS NOT NULL AND kind = 'fact'")
        .execute(db)
        .await
        .context("failed to clear memory_chunks after dimension change")?;
    Ok(())
}

/// Drop the embedding index (HNSW or IVFFlat).
pub async fn drop_vector_index(db: &PgPool) -> Result<()> {
    sqlx::query("DROP INDEX IF EXISTS idx_memory_embedding_hnsw")
        .execute(db)
        .await?;
    sqlx::query("DROP INDEX IF EXISTS idx_memory_embedding_ivfflat")
        .execute(db)
        .await?;
    Ok(())
}

/// Create IVFFlat index if it doesn't exist.
/// IVFFlat supports any dimension (unlike HNSW which caps at 4000 for halfvec).
pub async fn ensure_vector_index(db: &PgPool, dim: u32) -> Result<()> {
    // Drop old HNSW index if present (may fail on >4000 dims)
    if let Err(e) = sqlx::query("DROP INDEX IF EXISTS idx_memory_embedding_hnsw")
        .execute(db)
        .await
    {
        tracing::warn!(error = %e, "failed to drop old HNSW index (non-fatal)");
    }

    // pgvector index dimension limits:
    // - HNSW: max 4000 dims (halfvec) or 2000 (vector)
    // - IVFFlat: max 2000 dims
    // For models > 2000 dims (e.g. qwen3-embedding-8b at 4096), skip indexing.
    // Sequential scan works fine for <100K rows.
    if dim > 2000 {
        tracing::info!(dim, "embedding dim > 2000 — skipping vector index (sequential scan)");
        return Ok(());
    }

    // Count rows to determine lists parameter. IVFFlat requires rows >= lists.
    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks WHERE embedding IS NOT NULL")
        .fetch_one(db)
        .await
        .unwrap_or(0);

    if row_count < 10 {
        tracing::info!(rows = row_count, "skipping IVFFlat index — too few rows (need ≥10)");
        return Ok(());
    }

    // IVFFlat lists: sqrt(rows) clamped to [1, 1000]
    let lists = (row_count as f64).sqrt().ceil().clamp(1.0, 1000.0) as u32;
    let sql = format!(
        "CREATE INDEX IF NOT EXISTS idx_memory_embedding_ivfflat \
         ON memory_chunks USING ivfflat ((embedding::vector({dim})) vector_cosine_ops) \
         WITH (lists = {lists})"
    );
    sqlx::query(&sql)
        .execute(db)
        .await
        .context("failed to create IVFFlat index")?;
    Ok(())
}

// ── Search ───────────────────────────────────────────────────────────────────

/// Maximum number of pinned chunks fetched in one call.
/// `load_pinned` further trims by token budget; this is a hard ceiling so a
/// pathological agent with thousands of pinned chunks cannot OOM the Pi at
/// query time. 1000 is well above the realistic working set (typically
/// ~30-50 entries per agent) and far below DB-side memory limits.
///
/// Audit 2026-05-08 (4th pass): bumped from 500 → 1000 after noting that
/// shared chunks (scope='shared') compete with per-agent chunks for slots.
/// At 500, an agent with 30 own chunks plus a workspace with 480 shared
/// pins would silently lose half its agent-specific pinned context. 1000
/// restores headroom; we also emit a `warn!` when the limit is hit so the
/// operator can act before the cap becomes a real constraint.
pub const FETCH_PINNED_HARD_LIMIT: i64 = 1000;

/// Fetch up to `FETCH_PINNED_HARD_LIMIT` pinned chunks for a given agent,
/// ordered oldest first. Includes shared chunks (scope = 'shared') visible
/// to all agents. Used by L0 context loading — no embedding or search query
/// needed.
pub async fn fetch_pinned(db: &PgPool, agent_id: &str) -> Result<Vec<MemoryChunk>> {
    let rows = sqlx::query(
        r"SELECT id::text, content, COALESCE(source,'') AS source, pinned,
                  COALESCE(relevance_score, 1.0)::float8 AS relevance_score,
                  created_at, accessed_at, kind, importance
           FROM memory_chunks
           WHERE ($1 = '' OR agent_id = $1 OR scope = 'shared') AND pinned = true
             AND kind = 'fact'
           ORDER BY created_at ASC
           LIMIT $2",
    )
    .bind(agent_id)
    .bind(FETCH_PINNED_HARD_LIMIT)
    .fetch_all(db)
    .await
    .context("failed to fetch pinned memory chunks")?;

    if rows.len() == FETCH_PINNED_HARD_LIMIT as usize {
        tracing::warn!(
            agent_id = %agent_id,
            limit = FETCH_PINNED_HARD_LIMIT,
            "fetch_pinned reached LIMIT — older chunks may be silently truncated; \
             consider trimming pinned set or raising FETCH_PINNED_HARD_LIMIT",
        );
    }

    Ok(rows.iter().map(row_to_memory_chunk).collect())
}

/// Semantic similarity search: find nearest chunks by embedding cosine distance.
/// Filters by agent_id so that only the agent's own chunks (or shared chunks) are returned.
pub async fn search_semantic(
    db: &PgPool,
    vec_str: &str,
    candidate_limit: i64,
    agent_id: &str,
) -> Result<Vec<MemoryResult>> {
    let rows = sqlx::query(
        r"SELECT id::text,
                  content,
                  COALESCE(source, '') AS source,
                  pinned,
                  COALESCE(relevance_score, 1.0)::float8 AS relevance_score,
                  (1.0 - (embedding <=> $1::vector))::float8 AS similarity
           FROM memory_chunks
           WHERE embedding IS NOT NULL
             AND ($3 = '' OR agent_id = $3 OR scope = 'shared')
             AND kind = 'fact'
           ORDER BY embedding <=> $1::vector
           LIMIT $2",
    )
    .bind(vec_str)
    .bind(candidate_limit)
    .bind(agent_id)
    .fetch_all(db)
    .await
    .context("memory search query failed")?;

    Ok(rows.iter().map(row_to_memory_result).collect())
}

/// Build an OR-tsquery string from free-text input.
///
/// Splits on whitespace, strips tsquery operators (`! & | < > ( )`) from each
/// word, and joins remaining words with ` | `. Empty input → returns empty
/// string (caller should treat as "match nothing"). Used by the lenient FTS
/// fallback path when AND-mode search returns zero results.
fn or_tsquery_from(query: &str) -> String {
    query
        .split_whitespace()
        .map(|w| w.replace(['!', '&', '|', '<', '>', '(', ')', ':', '\''], ""))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Full-text search using `PostgreSQL` tsvector/tsquery.
/// Filters by agent_id so that only the agent's own chunks (or shared chunks) are returned.
///
/// `or_mode` = false: uses `plainto_tsquery` (AND across all words — strict).
/// `or_mode` = true:  uses `to_tsquery` with `|` (OR — lenient fallback).
pub async fn search_fts(
    db: &PgPool,
    query: &str,
    limit: i64,
    lang: &str,
    agent_id: &str,
) -> Result<Vec<MemoryResult>> {
    search_fts_inner(db, query, limit, lang, agent_id, false).await
}

/// Lenient FTS fallback: matches any of the query words (OR), not all (AND).
/// Use only when strict AND-mode search returns empty AND semantic also failed.
pub async fn search_fts_or(
    db: &PgPool,
    query: &str,
    limit: i64,
    lang: &str,
    agent_id: &str,
) -> Result<Vec<MemoryResult>> {
    search_fts_inner(db, query, limit, lang, agent_id, true).await
}

/// Validate a `pg_trgm` similarity threshold before it reaches Postgres.
///
/// pg_trgm's `set_limit()` (and `SET pg_trgm.similarity_threshold = ...`)
/// rejects NaN, ±Inf, and values outside `[0.0, 1.0]` with a cryptic error
/// that doesn't surface the bad input. We catch here for two reasons:
/// 1. Clearer operator-facing error message (logs the actual bad value).
/// 2. Avoids a no-op transaction round-trip + connection-state surprise
///    if a future refactor exposes `threshold` to user/config input.
pub(crate) fn validate_trgm_threshold(threshold: f32) -> Result<()> {
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        anyhow::bail!(
            "pg_trgm similarity threshold must be a finite number in [0.0, 1.0], got: {threshold}"
        );
    }
    Ok(())
}

/// Trigram similarity search using pg_trgm.
///
/// Uses operator `%` so the GIN index `idx_memory_chunks_content_trgm`
/// (gin_trgm_ops) is actually used. The threshold is set per-transaction via
/// `SET LOCAL pg_trgm.similarity_threshold = <value>` immediately before the
/// SELECT. `SET LOCAL` is transaction-scoped — auto-cleaned on commit OR
/// rollback, so a query error cannot leak the GUC onto the pooled connection.
///
/// `agent_id`: filter to agent's own chunks plus shared chunks. Empty string = no filter.
///
/// Returns `Err` (without opening a transaction) when `threshold` is NaN,
/// ±Inf, or outside `[0.0, 1.0]`. Today the only call site passes a hardcoded
/// `0.3` const, but the guard exists so future config-driven thresholds
/// fail-fast with a clear error instead of an opaque pg_trgm rejection.
pub async fn search_trigram(
    db: &PgPool,
    query: &str,
    limit: i64,
    threshold: f32,
    agent_id: &str,
) -> Result<Vec<MemoryResult>> {
    validate_trgm_threshold(threshold)?;

    if query.trim().is_empty() {
        return Ok(vec![]);
    }

    // SET LOCAL + select must run on the same connection — use a transaction.
    let mut tx = db.begin().await.context("begin trigram tx")?;

    // Postgres parses `SET` before parameter binding, so $1 doesn't work here.
    // Inline the value via format!() — `threshold` is validated above (finite,
    // in [0.0, 1.0]), so `format!("{f32}")` produces a Postgres-safe literal.
    let sql = format!("SET LOCAL pg_trgm.similarity_threshold = {threshold}");
    sqlx::query(&sql)
        .execute(&mut *tx)
        .await
        .context("set local similarity_threshold")?;

    let rows = sqlx::query(
        r"SELECT id::text,
                  content,
                  COALESCE(source, '') AS source,
                  pinned,
                  COALESCE(relevance_score, 1.0)::float8 AS relevance_score,
                  similarity(content, $1)::float8 AS similarity
           FROM memory_chunks
           WHERE content % $1
             AND ($3 = '' OR agent_id = $3 OR scope = 'shared')
             AND kind = 'fact'
           ORDER BY similarity DESC
           LIMIT $2",
    )
    .bind(query)
    .bind(limit)
    .bind(agent_id)
    .fetch_all(&mut *tx)
    .await
    .context("trigram search query failed")?;

    tx.commit().await.context("commit trigram tx")?;

    Ok(rows.iter().map(row_to_memory_result).collect())
}

async fn search_fts_inner(
    db: &PgPool,
    query: &str,
    limit: i64,
    lang: &str,
    agent_id: &str,
    or_mode: bool,
) -> Result<Vec<MemoryResult>> {
    // `lang` is bound as `$4::regconfig` (Postgres validates the dictionary
    // name); `tsquery_fn` is a Rust-controlled string literal, not input.
    let (effective_query, tsquery_fn) = if or_mode {
        let or_q = or_tsquery_from(query);
        if or_q.is_empty() {
            return Ok(vec![]);
        }
        (or_q, "to_tsquery")
    } else {
        (query.to_string(), "plainto_tsquery")
    };
    let sql = format!(
        r"SELECT id::text,
                  content,
                  COALESCE(source, '') AS source,
                  pinned,
                  COALESCE(relevance_score, 1.0)::float8 AS relevance_score,
                  ts_rank_cd(tsv, {tsquery_fn}($4::regconfig, $1))::float8 AS similarity
           FROM memory_chunks
           WHERE tsv @@ {tsquery_fn}($4::regconfig, $1)
             AND ($3 = '' OR agent_id = $3 OR scope = 'shared')
             AND kind = 'fact'
           ORDER BY ts_rank_cd(tsv, {tsquery_fn}($4::regconfig, $1)) DESC,
                    relevance_score DESC
           LIMIT $2",
    );

    let rows = sqlx::query(&sql)
        .bind(&effective_query)
        .bind(limit)
        .bind(agent_id)
        .bind(lang)
        .fetch_all(db)
        .await
        .map_err(|e| map_fts_lang_error(lang, e))
        .context("FTS search query failed")?;

    Ok(rows.iter().map(row_to_memory_result).collect())
}

/// Update `accessed_at` timestamp for the given chunk IDs.
pub async fn touch_accessed(db: &PgPool, ids: &[uuid::Uuid]) {
    if ids.is_empty() {
        return;
    }
    if let Err(e) = sqlx::query(
        "UPDATE memory_chunks SET accessed_at = now() WHERE id = ANY($1)",
    )
    .bind(ids)
    .execute(db)
    .await
    {
        tracing::warn!(count = ids.len(), error = %e, "failed to update accessed_at on memory chunks");
    }
}

/// Return the most-recently-accessed memory chunks (pinned first).
pub async fn fetch_recent(db: &PgPool, limit: i64) -> Result<Vec<MemoryResult>> {
    let rows = sqlx::query(
        r"SELECT id::text,
                  content,
                  COALESCE(source, '') AS source,
                  pinned,
                  COALESCE(relevance_score, 1.0)::float8 AS relevance_score,
                  1.0::float8 AS similarity
           FROM memory_chunks
           ORDER BY pinned DESC, COALESCE(accessed_at, created_at) DESC
           LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await
    .context("recent memory query failed")?;

    Ok(rows.iter().map(row_to_memory_result).collect())
}

// ── Index ────────────────────────────────────────────────────────────────────

/// Shared INSERT body used by both pool and transaction entry points.
///
/// Generic over `Executor<'e, Database = Postgres>` so the same SQL path
/// runs on `&PgPool` and `&mut Transaction`. Lang is bound as `regconfig`
/// — invalid values surface as SQLSTATE `22023` and are mapped to a clear
/// domain error.
///
/// History:
///   m032 dropped category ($9) and topic ($10) — scope shifted $11 → $9
///   m033 dropped parent_id ($7) and chunk_index ($8) — scope shifted $9 → $7
/// Current: 9 columns, 7 unique binds; lang at $8 is the regconfig.
#[allow(clippy::too_many_arguments)]
async fn insert_chunk_inner<'e, E>(
    executor: E,
    id: &str,
    content: &str,
    vec_str: &str,
    source: &str,
    pinned: bool,
    lang: &str,
    scope: &str,
    agent_id: &str,
    kind: &str,
    importance: f32,
    lineage: Option<&[uuid::Uuid]>,
    valence: Option<f32>,
) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(INSERT_CHUNK_SQL)
        .bind(id)           // $1
        .bind(agent_id)     // $2
        .bind(content)      // $3
        .bind(vec_str)      // $4 (embedding)
        .bind(source)       // $5
        .bind(pinned)       // $6
        .bind(scope)        // $7
        .bind(lang)         // $8 (regconfig)
        .bind(kind)         // $9
        .bind(importance)   // $10
        .bind(lineage)      // $11
        .bind(valence)      // $12
        .execute(executor)
        .await
        .map_err(|e| map_fts_lang_error(lang, e))
        .context("failed to insert memory chunk")?;
    Ok(())
}

/// Insert a new memory chunk with embedding and FTS tsvector.
#[allow(clippy::too_many_arguments)]
pub async fn insert_chunk(
    db: &PgPool,
    id: &str,
    content: &str,
    vec_str: &str,
    source: &str,
    pinned: bool,
    lang: &str,
    scope: &str,
    agent_id: &str,
    kind: &str,
    importance: f32,
    lineage: Option<&[uuid::Uuid]>,
    valence: Option<f32>,
) -> Result<()> {
    insert_chunk_inner(db, id, content, vec_str, source, pinned, lang, scope, agent_id, kind, importance, lineage, valence).await
}

/// Insert a new memory chunk within an existing transaction.
/// Identical SQL path to `insert_chunk`, but executes on a transaction handle.
#[allow(clippy::too_many_arguments)]
pub async fn insert_chunk_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    content: &str,
    vec_str: &str,
    source: &str,
    pinned: bool,
    lang: &str,
    scope: &str,
    agent_id: &str,
    kind: &str,
    importance: f32,
    lineage: Option<&[uuid::Uuid]>,
    valence: Option<f32>,
) -> Result<()> {
    insert_chunk_inner(&mut **tx, id, content, vec_str, source, pinned, lang, scope, agent_id, kind, importance, lineage, valence).await
}

// ── Get ──────────────────────────────────────────────────────────────────────

/// Retrieve a single chunk by ID.
pub async fn get_chunk_by_id(db: &PgPool, id: &str) -> Result<Vec<MemoryChunk>> {
    let rows = sqlx::query(
        r"SELECT id::text, content, COALESCE(source,'') AS source, pinned,
                  COALESCE(relevance_score,1.0)::float8 AS relevance_score,
                  created_at, accessed_at, kind, importance
           FROM memory_chunks WHERE id = $1::uuid",
    )
    .bind(id)
    .fetch_all(db)
    .await?;

    Ok(rows.iter().map(row_to_memory_chunk).collect())
}

/// Retrieve chunks by source, ordered by creation date.
pub async fn get_chunks_by_source(
    db: &PgPool,
    source: &str,
    limit: i64,
) -> Result<Vec<MemoryChunk>> {
    let rows = sqlx::query(
        r"SELECT id::text, content, COALESCE(source,'') AS source, pinned,
                  COALESCE(relevance_score,1.0)::float8 AS relevance_score,
                  created_at, accessed_at, kind, importance
           FROM memory_chunks WHERE source = $1
           ORDER BY created_at DESC LIMIT $2",
    )
    .bind(source)
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows.iter().map(row_to_memory_chunk).collect())
}

/// Retrieve most recently accessed chunks.
pub async fn get_chunks_recent(db: &PgPool, limit: i64) -> Result<Vec<MemoryChunk>> {
    let rows = sqlx::query(
        r"SELECT id::text, content, COALESCE(source,'') AS source, pinned,
                  COALESCE(relevance_score,1.0)::float8 AS relevance_score,
                  created_at, accessed_at, kind, importance
           FROM memory_chunks
           ORDER BY accessed_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;

    Ok(rows.iter().map(row_to_memory_chunk).collect())
}

// ── Delete / Rebuild ─────────────────────────────────────────────────────────

/// Rebuild all tsv columns with the given FTS language.
pub async fn rebuild_fts(db: &PgPool, lang: &str) -> Result<u64> {
    // `lang` is bound as `$1::regconfig`; Postgres validates the dictionary
    // name and raises SQLSTATE 22023 if it's unknown.
    let res = sqlx::query("UPDATE memory_chunks SET tsv = to_tsvector($1::regconfig, content)")
        .bind(lang)
        .execute(db)
        .await
        .map_err(|e| map_fts_lang_error(lang, e))
        .context("failed to rebuild FTS index")?;
    Ok(res.rows_affected())
}

/// Delete a memory chunk by id.
pub async fn delete_chunk(db: &PgPool, chunk_id: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM memory_chunks WHERE id = $1::uuid")
        .bind(chunk_id)
        .execute(db)
        .await
        .context("failed to delete memory chunk")?;
    Ok(res.rows_affected() > 0)
}

/// Delete all chunks with a given source (e.g. filename).
/// F-09: guard with `kind='fact' OR kind IS NULL` so this 5th hard-delete path
/// can never purge a soul biography row (event/reflection) that happened to
/// share a source string — consistent with the other 4 documented paths.
pub async fn delete_by_source(db: &PgPool, source: &str) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM memory_chunks WHERE source = $1 AND (kind = 'fact' OR kind IS NULL)",
    )
    .bind(source)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Wipe all memory for an agent.
pub async fn wipe_agent_memory(db: &PgPool, agent_id: &str) -> Result<u64> {
    // Biography (kind event/reflection) is immortal via routine paths — deliberate
    // removal is the raw-SQL quarantine runbook only. This admin wipe spares it,
    // matching run_memory_decay / cleanup / reindex-purge / clear_embeddings.
    let result = sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1 AND kind = 'fact'")
        .bind(agent_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

/// Insert a reindex task into the memory worker queue.
pub async fn enqueue_reindex_task(db: &PgPool, params: serde_json::Value) -> Result<uuid::Uuid> {
    sqlx::query_scalar(
        "INSERT INTO memory_tasks (task_type, params) VALUES ('reindex', $1) RETURNING id",
    )
    .bind(params)
    .fetch_one(db)
    .await
    .context("failed to enqueue reindex task")
}

// ── Soul (autobiographical memory) ──────────────────────────────────────────

/// Candidate row for soul retrieval scoring (recency×importance×relevance in Rust).
#[derive(Debug, Clone)]
pub struct SoulCandidate {
    pub id: uuid::Uuid,
    pub content: String,
    pub source: String,
    pub kind: String,
    pub importance: f32,
    pub created_at: DateTime<Utc>,
    pub similarity: f64,
    /// Per-chunk emotional valence ([-1,1]); NULL for facts/reflections/legacy.
    /// Used for mood-congruence bias in soul retrieval scoring (feature #5).
    pub valence: Option<f32>,
}

fn row_to_soul_candidate(r: &sqlx::postgres::PgRow) -> SoulCandidate {
    use sqlx::Row;
    SoulCandidate {
        id: r.get("id"),
        content: r.get("content"),
        source: r.get("source"),
        kind: r.get("kind"),
        importance: r.get("importance"),
        created_at: r.get("created_at"),
        similarity: r.try_get("similarity").unwrap_or(0.0),
        valence: r.try_get("valence").ok().flatten(),
    }
}

/// Top-N soul chunks (event/reflection) by cosine distance. No touch_accessed —
/// soul recency is computed from created_at (spec §1: write-on-read disabled).
/// ANN candidate pool for soul retrieval. Reflections are pulled in a SEPARATE
/// subquery with its own `reflection_limit` and UNION'd with the top-`event_limit`
/// events, so the rare durable (reflection) layer is never crowded out of the pool
/// by the far more numerous events before Rust-side scoring runs. Kinds are
/// disjoint, so the union has no duplicates. Order is irrelevant here —
/// `score_and_select` reranks the combined pool.
pub async fn soul_candidates(
    db: &PgPool,
    vec_str: &str,
    agent_id: &str,
    exclude_source: Option<&str>,
    event_limit: i64,
    reflection_limit: i64,
) -> Result<Vec<SoulCandidate>> {
    let rows = sqlx::query(
        r"(SELECT id, content, COALESCE(source,'') AS source, kind, importance,
                  created_at, valence,
                  (1.0 - (embedding <=> $1::vector))::float8 AS similarity
           FROM memory_chunks
           WHERE embedding IS NOT NULL
             AND agent_id = $2
             AND kind = 'event'
             AND ($3::text IS NULL OR $3::text = '' OR source <> $3)
           ORDER BY embedding <=> $1::vector
           LIMIT $4)
          UNION ALL
          (SELECT id, content, COALESCE(source,'') AS source, kind, importance,
                  created_at, valence,
                  (1.0 - (embedding <=> $1::vector))::float8 AS similarity
           FROM memory_chunks
           WHERE embedding IS NOT NULL
             AND agent_id = $2
             AND kind = 'reflection'
             AND ($3::text IS NULL OR $3::text = '' OR source <> $3)
           ORDER BY embedding <=> $1::vector
           LIMIT $5)",
    )
    .bind(vec_str)
    .bind(agent_id)
    .bind(exclude_source)
    .bind(event_limit)
    .bind(reflection_limit)
    .fetch_all(db)
    .await
    .context("soul candidates query failed")?;
    Ok(rows.iter().map(row_to_soul_candidate).collect())
}

/// Marker of the last successfully committed reflection cycle (spec §3):
/// reflections are written in one transaction, so MAX(created_at) is safe.
pub async fn latest_reflection_at(db: &PgPool, agent_id: &str) -> Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar(
        "SELECT MAX(created_at) FROM memory_chunks WHERE agent_id = $1 AND kind = 'reflection'",
    )
    .bind(agent_id)
    .fetch_one(db)
    .await
    .context("latest_reflection_at query failed")
}

/// (source, importance) of events created after `since` (all events when None).
/// Per-session contribution capping happens in Rust (spec §3).
pub async fn event_importance_since(
    db: &PgPool,
    agent_id: &str,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<(String, f32)>> {
    let rows: Vec<(String, f32)> = sqlx::query_as(
        r"SELECT COALESCE(source,''), importance FROM memory_chunks
           WHERE agent_id = $1 AND kind = 'event'
             AND ($2::timestamptz IS NULL OR created_at > $2)",
    )
    .bind(agent_id)
    .bind(since)
    .fetch_all(db)
    .await
    .context("event_importance_since query failed")?;
    Ok(rows)
}

/// Freshest soul chunks for the reflection window (created_at DESC).
pub async fn recent_soul_chunks(db: &PgPool, agent_id: &str, limit: i64) -> Result<Vec<SoulCandidate>> {
    let rows = sqlx::query(
        r"SELECT id, content, COALESCE(source,'') AS source, kind, importance,
                  created_at, valence, 0.0::float8 AS similarity
           FROM memory_chunks
           WHERE agent_id = $1 AND kind IN ('event', 'reflection')
           ORDER BY created_at DESC
           LIMIT $2",
    )
    .bind(agent_id)
    .bind(limit)
    .fetch_all(db)
    .await
    .context("recent_soul_chunks query failed")?;
    Ok(rows.iter().map(row_to_soul_candidate).collect())
}

/// Freshest open-thread chunk contents for an agent (spec §3.2).
/// Prefix-scan on source over idx_memory_source; recency window in days.
pub async fn recent_open_thread_chunks(
    db: &PgPool,
    agent_id: &str,
    since_days: i64,
    limit: i64,
) -> Result<Vec<String>> {
    // bind days as i32 to match house style (make_interval `days` is int4;
    // every existing make_interval call site binds i32 — see usage.rs, sessions.rs).
    let rows: Vec<String> = sqlx::query_scalar(
        r"SELECT content FROM memory_chunks
           WHERE agent_id = $1
             AND source LIKE 'open_thread:%'
             AND created_at > now() - make_interval(days => $2)
           ORDER BY created_at DESC
           LIMIT $3",
    )
    .bind(agent_id)
    .bind(since_days as i32)
    .bind(limit)
    .fetch_all(db)
    .await
    .context("recent_open_thread_chunks query failed")?;
    Ok(rows)
}

/// kind of a single chunk (None when the id does not exist / is not a UUID).
pub async fn chunk_kind(db: &PgPool, id: &str) -> Result<Option<String>> {
    let Ok(uuid) = id.parse::<uuid::Uuid>() else { return Ok(None) };
    sqlx::query_scalar("SELECT kind FROM memory_chunks WHERE id = $1")
        .bind(uuid)
        .fetch_optional(db)
        .await
        .context("chunk_kind query failed")
}

#[cfg(test)]
mod tests {
    // ── SQL structure tests ─────────────────────────────────────────
    // These catch the class of bugs where column removal breaks INSERT/SELECT
    // (like the user_id NOT NULL bug caught in production).

    // ── pg_trgm threshold validation ────────────────────────────────

    #[test]
    fn validate_trgm_threshold_accepts_in_range() {
        assert!(super::validate_trgm_threshold(0.0).is_ok());
        assert!(super::validate_trgm_threshold(0.3).is_ok());
        assert!(super::validate_trgm_threshold(0.5).is_ok());
        assert!(super::validate_trgm_threshold(1.0).is_ok());
    }

    #[test]
    fn validate_trgm_threshold_rejects_nan() {
        let err = super::validate_trgm_threshold(f32::NAN).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("NaN") || msg.contains("nan") || msg.contains("finite"));
    }

    #[test]
    fn validate_trgm_threshold_rejects_infinity() {
        assert!(super::validate_trgm_threshold(f32::INFINITY).is_err());
        assert!(super::validate_trgm_threshold(f32::NEG_INFINITY).is_err());
    }

    #[test]
    fn validate_trgm_threshold_rejects_out_of_range() {
        // Below 0.0
        assert!(super::validate_trgm_threshold(-0.0001).is_err());
        assert!(super::validate_trgm_threshold(-1.0).is_err());
        // Above 1.0
        assert!(super::validate_trgm_threshold(1.0001).is_err());
        assert!(super::validate_trgm_threshold(2.0).is_err());
        // Subnormal-but-still-out-of-range edge
        assert!(super::validate_trgm_threshold(f32::MAX).is_err());
        assert!(super::validate_trgm_threshold(f32::MIN).is_err());
    }

    #[test]
    fn validate_trgm_threshold_error_includes_bad_value() {
        // Operator-facing error must surface the actual bad input so logs
        // are diagnostic, not "set_limit failed" with no context.
        let err = super::validate_trgm_threshold(2.5).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("2.5"), "error must include the bad value, got: {msg}");
    }

    #[test]
    fn or_tsquery_joins_words_with_pipe() {
        assert_eq!(super::or_tsquery_from("foo bar baz"), "foo | bar | baz");
    }

    #[test]
    fn or_tsquery_strips_tsquery_operators() {
        // Characters ! & | < > ( ) : ' are stripped
        assert_eq!(super::or_tsquery_from("foo & bar | baz"), "foo | bar | baz");
    }

    #[test]
    fn or_tsquery_empty_input_returns_empty() {
        assert_eq!(super::or_tsquery_from(""), "");
    }

    #[test]
    fn or_tsquery_single_word() {
        assert_eq!(super::or_tsquery_from("hello"), "hello");
    }

    #[test]
    fn or_tsquery_filters_empty_tokens_after_strip() {
        // A lone operator becomes empty after stripping, should be filtered
        assert_eq!(super::or_tsquery_from("foo & bar"), "foo | bar");
    }

    #[test]
    fn or_tsquery_embedded_operators_within_word_are_stripped() {
        // "foo&bar" → split_whitespace → ["foo&bar"] → strip '&' → "foobar" → "foobar"
        // Distinct from "foo & bar" where '&' is a separate token
        assert_eq!(super::or_tsquery_from("foo&bar"), "foobar");
    }

    // ── INSERT helper integration tests (T1) ────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_insert_chunk_with_russian_lang_succeeds(pool: sqlx::PgPool) {
        let id = uuid::Uuid::new_v4().to_string();
        let result = super::insert_chunk(
            &pool,
            &id,
            "тестовое содержимое",
            "[0.0,0.0,0.0,0.0]",
            "test_source",
            false,
            "russian",
            "shared",
            "test_agent",
            "fact",
            5.0,
            None,
            None,
        ).await;
        assert!(result.is_ok(), "russian lang insert failed: {:?}", result);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_insert_chunk_with_english_lang_succeeds(pool: sqlx::PgPool) {
        let id = uuid::Uuid::new_v4().to_string();
        let result = super::insert_chunk(
            &pool,
            &id,
            "english test content",
            "[0.0,0.0,0.0,0.0]",
            "test_source",
            false,
            "english",
            "shared",
            "test_agent",
            "fact",
            5.0,
            None,
            None,
        ).await;
        assert!(result.is_ok(), "english lang insert failed: {:?}", result);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_insert_chunk_invalid_lang_returns_domain_error(pool: sqlx::PgPool) {
        let id = uuid::Uuid::new_v4().to_string();
        let result = super::insert_chunk(
            &pool,
            &id,
            "content",
            "[0.0,0.0,0.0,0.0]",
            "test_source",
            false,
            "klingon",
            "shared",
            "test_agent",
            "fact",
            5.0,
            None,
            None,
        ).await;
        assert!(result.is_err(), "klingon should be rejected");
        // Use {:#} so anyhow's alternate display walks the full chain — the
        // outer context ("failed to insert memory chunk") wraps the inner
        // domain error from map_fts_lang_error.
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("invalid FTS language"), "expected domain error, got: {err}");
        assert!(err.contains("klingon"), "expected error to include offending value, got: {err}");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_insert_chunk_sql_injection_attempt_returns_error(pool: sqlx::PgPool) {
        let id = uuid::Uuid::new_v4().to_string();
        let attack = "'; DROP TABLE memory_chunks --";
        let result = super::insert_chunk(
            &pool,
            &id,
            "content",
            "[0.0,0.0,0.0,0.0]",
            "test_source",
            false,
            attack,
            "shared",
            "test_agent",
            "fact",
            5.0,
            None,
            None,
        ).await;
        assert!(result.is_err(), "injection attempt should be rejected");
        let count: (i64,) = sqlx::query_as("SELECT count(*) FROM memory_chunks")
            .fetch_one(&pool).await.expect("table memory_chunks still exists");
        let _ = count;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_insert_chunk_tx_uses_same_sql_path(pool: sqlx::PgPool) {
        let id = uuid::Uuid::new_v4().to_string();
        let mut tx = pool.begin().await.unwrap();
        let result = super::insert_chunk_tx(
            &mut tx,
            &id,
            "tx test content",
            "[0.0,0.0,0.0,0.0]",
            "test_source",
            false,
            "russian",
            "shared",
            "test_agent",
            "fact",
            5.0,
            None,
            None,
        ).await;
        assert!(result.is_ok(), "tx insert failed: {:?}", result);
        tx.commit().await.unwrap();
        let row: (String,) = sqlx::query_as("SELECT content FROM memory_chunks WHERE id = $1::uuid")
            .bind(&id).fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, "tx test content");
    }

    // ── Soul kind-filtering tests (T2) ───────────────────────────────

    async fn insert_soul_row(pool: &sqlx::PgPool, agent: &str, kind: &str, source: &str, importance: f32) -> uuid::Uuid {
        sqlx::query_scalar(
            "INSERT INTO memory_chunks (id, agent_id, content, embedding, source, pinned, scope, kind, importance, tsv) \
             VALUES (gen_random_uuid(), $1, 'событие тест', '[0.5,0.5,0.5,0.5]'::vector, $2, false, 'private', $3, $4, to_tsvector('simple','событие тест')) \
             RETURNING id",
        )
        .bind(agent).bind(source).bind(kind).bind(importance)
        .fetch_one(pool).await.unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn generic_search_paths_exclude_soul_kinds(pool: sqlx::PgPool) {
        insert_soul_row(&pool, "A", "event", "soul_event:s1", 9.0).await;
        insert_soul_row(&pool, "A", "reflection", "soul_reflection", 9.0).await;
        insert_soul_row(&pool, "A", "fact", "manual", 5.0).await;

        let sem = super::search_semantic(&pool, "[0.5,0.5,0.5,0.5]", 50, "A").await.unwrap();
        assert!(sem.iter().all(|r| r.source == "manual"), "semantic must only see kind='fact'");

        let fts = super::search_fts(&pool, "событие", 50, "simple", "A").await.unwrap();
        assert!(fts.iter().all(|r| r.source == "manual"), "fts must only see kind='fact'");

        let fts_or = super::search_fts_or(&pool, "событие тест", 50, "simple", "A").await.unwrap();
        assert!(fts_or.iter().all(|r| r.source == "manual"), "fts_or must only see kind='fact'");

        let trgm = super::search_trigram(&pool, "событие", 50, 0.1, "A").await.unwrap();
        assert!(trgm.iter().all(|r| r.source == "manual"), "trigram must only see kind='fact'");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn fetch_pinned_excludes_soul_kinds(pool: sqlx::PgPool) {
        sqlx::query(
            "INSERT INTO memory_chunks (id, agent_id, content, source, pinned, scope, kind) \
             VALUES (gen_random_uuid(), 'A', 'pinned event', 'soul_event:s', true, 'private', 'event')",
        ).execute(&pool).await.unwrap();
        let pinned = super::fetch_pinned(&pool, "A").await.unwrap();
        assert!(pinned.is_empty(), "pinned soul chunk must not enter L0");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn soul_candidates_filters_agent_kind_and_exclude_source(pool: sqlx::PgPool) {
        insert_soul_row(&pool, "A", "event", "soul_event:s1", 7.0).await;
        insert_soul_row(&pool, "A", "event", "soul_event:s2", 7.0).await;
        insert_soul_row(&pool, "A", "fact", "manual", 5.0).await;
        insert_soul_row(&pool, "B", "event", "soul_event:s3", 7.0).await;

        let all = super::soul_candidates(&pool, "[0.5,0.5,0.5,0.5]", "A", None, 50, 15).await.unwrap();
        assert_eq!(all.len(), 2);
        let excl = super::soul_candidates(&pool, "[0.5,0.5,0.5,0.5]", "A", Some("soul_event:s1"), 50, 15).await.unwrap();
        assert_eq!(excl.len(), 1);
        assert_eq!(excl[0].source, "soul_event:s2");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn soul_candidates_reflection_floor_survives_event_flood(pool: sqlx::PgPool) {
        // 5 events + 2 reflections; event_limit=3 would crowd reflections out of a
        // single top-N pool. The separate reflection subquery must still surface both.
        for i in 0..5 {
            insert_soul_row(&pool, "A", "event", &format!("soul_event:s{i}"), 7.0).await;
        }
        insert_soul_row(&pool, "A", "reflection", "soul_reflection", 9.0).await;
        insert_soul_row(&pool, "A", "reflection", "soul_reflection", 8.0).await;

        let cands = super::soul_candidates(&pool, "[0.5,0.5,0.5,0.5]", "A", None, 3, 15).await.unwrap();
        let events = cands.iter().filter(|c| c.kind == "event").count();
        let reflections = cands.iter().filter(|c| c.kind == "reflection").count();
        assert_eq!(events, 3, "events capped at event_limit");
        assert_eq!(reflections, 2, "both reflections survive despite the event flood");
    }

    // ── Open-thread recency tests (T2b) ──────────────────────────────

    async fn insert_open_thread(pool: &sqlx::PgPool, agent: &str, content: &str, age_days: i32) {
        sqlx::query(
            "INSERT INTO memory_chunks (id, agent_id, content, source, pinned, scope, kind, created_at) \
             VALUES (gen_random_uuid(), $1, $2, 'open_thread:s1', false, 'private', 'fact', \
                     now() - make_interval(days => $3))",
        )
        .bind(agent).bind(content).bind(age_days)
        .execute(pool).await.unwrap();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recent_open_threads_filters_window_agent_and_prefix(pool: sqlx::PgPool) {
        insert_open_thread(&pool, "A", "свежий тред", 0).await;
        insert_open_thread(&pool, "A", "старый тред", 30).await;
        insert_open_thread(&pool, "B", "чужой тред", 0).await;
        // non-open_thread fact for agent A must not match the prefix
        sqlx::query(
            "INSERT INTO memory_chunks (id, agent_id, content, source, pinned, scope, kind) \
             VALUES (gen_random_uuid(), 'A', 'обычный факт', 'manual', false, 'private', 'fact')",
        ).execute(&pool).await.unwrap();

        let got = super::recent_open_thread_chunks(&pool, "A", 5, 10).await.unwrap();
        assert_eq!(got, vec!["свежий тред".to_string()]);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recent_open_threads_limit_and_order(pool: sqlx::PgPool) {
        insert_open_thread(&pool, "A", "тред старее", 3).await;
        insert_open_thread(&pool, "A", "тред средний", 2).await;
        insert_open_thread(&pool, "A", "тред новейший", 1).await;
        let got = super::recent_open_thread_chunks(&pool, "A", 5, 2).await.unwrap();
        assert_eq!(got, vec!["тред новейший".to_string(), "тред средний".to_string()]);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn migration_defaults_cover_legacy_rows(pool: sqlx::PgPool) {
        // Строка «до-076 формы» (kind/importance/lineage не указаны) получает дефолты —
        // эмуляция legacy-данных, по которым прокатилась миграция (спека §9).
        sqlx::query(
            "INSERT INTO memory_chunks (id, agent_id, content, source, pinned, scope) \
             VALUES (gen_random_uuid(), 'A', 'legacy', 'manual', false, 'private')",
        ).execute(&pool).await.unwrap();
        let (kind, importance): (String, f32) = sqlx::query_as(
            "SELECT kind, importance FROM memory_chunks WHERE agent_id = 'A'",
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(kind, "fact");
        assert_eq!(importance, 5.0);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn reflection_marker_and_event_counter(pool: sqlx::PgPool) {
        assert!(super::latest_reflection_at(&pool, "A").await.unwrap().is_none());
        insert_soul_row(&pool, "A", "event", "soul_event:s1", 8.0).await;
        insert_soul_row(&pool, "A", "event", "soul_event:s1", 6.0).await;
        let pairs = super::event_importance_since(&pool, "A", None).await.unwrap();
        assert_eq!(pairs.len(), 2);
        insert_soul_row(&pool, "A", "reflection", "soul_reflection", 7.0).await;
        assert!(super::latest_reflection_at(&pool, "A").await.unwrap().is_some());
    }
}
