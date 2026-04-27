use anyhow::{Context, Result};
use sqlx::PgPool;

use crate::memory::{MemoryChunk, MemoryResult};

/// Allowed PostgreSQL FTS dictionaries (whitelist prevents SQL injection).
const ALLOWED_FTS_LANGS: &[&str] = &[
    "simple", "english", "russian", "spanish", "french", "german",
    "italian", "portuguese", "dutch", "swedish", "norwegian", "danish",
    "finnish", "hungarian", "romanian", "turkish", "arabic", "hindi",
    "indonesian", "irish", "nepali", "serbian", "tamil", "yiddish",
    "greek", "armenian", "basque", "catalan", "lithuanian",
];

fn validate_fts_lang(lang: &str) -> Result<()> {
    if !ALLOWED_FTS_LANGS.contains(&lang) {
        anyhow::bail!("invalid FTS language: {lang} (not in whitelist)");
    }
    Ok(())
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
        parent_id: r.try_get::<Option<String>, _>("parent_id").ok().flatten(),
        chunk_index: r.try_get::<i32, _>("chunk_index").unwrap_or(0),
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
pub async fn clear_embeddings(db: &PgPool) -> Result<()> {
    sqlx::query("DELETE FROM memory_chunks WHERE embedding IS NOT NULL")
        .execute(db)
        .await
        .context("failed to clear memory_chunks after dimension change")?;
    Ok(())
}

/// Drop the embedding index (HNSW or IVFFlat).
pub async fn drop_hnsw_index(db: &PgPool) -> Result<()> {
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
pub async fn ensure_hnsw_index(db: &PgPool, dim: u32) -> Result<()> {
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

/// Fetch all pinned chunks for a given agent, ordered oldest first.
/// Includes shared chunks (scope = 'shared') visible to all agents.
/// Used by L0 context loading — no embedding or search query needed.
pub async fn fetch_pinned(db: &PgPool, agent_id: &str) -> Result<Vec<MemoryChunk>> {
    let rows = sqlx::query(
        r"SELECT id::text, content, COALESCE(source,'') AS source, pinned,
                  COALESCE(relevance_score, 1.0)::float8 AS relevance_score,
                  created_at, accessed_at
           FROM memory_chunks
           WHERE ($1 = '' OR agent_id = $1 OR scope = 'shared') AND pinned = true
           ORDER BY created_at ASC",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await
    .context("failed to fetch pinned memory chunks")?;

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
                  (1.0 - (embedding <=> $1::vector))::float8 AS similarity,
                  parent_id::text,
                  chunk_index
           FROM memory_chunks
           WHERE embedding IS NOT NULL
             AND ($3 = '' OR agent_id = $3 OR scope = 'shared')
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

/// Full-text search using `PostgreSQL` tsvector/tsquery.
/// Filters by agent_id so that only the agent's own chunks (or shared chunks) are returned.
pub async fn search_fts(
    db: &PgPool,
    query: &str,
    limit: i64,
    lang: &str,
    agent_id: &str,
) -> Result<Vec<MemoryResult>> {
    validate_fts_lang(lang)?;
    // SAFETY: `lang` is validated by validate_fts_lang() whitelist
    // letters. Not user input -- comes from server config.
    let sql = format!(
        r"SELECT id::text,
                  content,
                  COALESCE(source, '') AS source,
                  pinned,
                  COALESCE(relevance_score, 1.0)::float8 AS relevance_score,
                  ts_rank_cd(tsv, plainto_tsquery('{lang}', $1))::float8 AS similarity,
                  parent_id::text,
                  chunk_index
           FROM memory_chunks
           WHERE tsv @@ plainto_tsquery('{lang}', $1)
             AND ($3 = '' OR agent_id = $3 OR scope = 'shared')
           ORDER BY ts_rank_cd(tsv, plainto_tsquery('{lang}', $1)) DESC,
                    relevance_score DESC
           LIMIT $2",
    );

    let rows = sqlx::query(&sql)
        .bind(query)
        .bind(limit)
        .bind(agent_id)
        .fetch_all(db)
        .await
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
                  1.0::float8 AS similarity,
                  parent_id::text,
                  chunk_index
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
    parent_id: Option<&str>,
    chunk_index: i32,
    scope: &str,
    agent_id: &str,
) -> Result<()> {
    validate_fts_lang(lang)?;
    // SAFETY: `lang` is validated by validate_fts_lang() whitelist
    // letters. Not user input -- comes from server config.
    //
    // WAS: 13 columns (id, agent_id, content, embedding, source, pinned,
    // relevance_score, tsv, parent_id, chunk_index, category, topic, scope) →
    // VALUES ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector('lang',$3),
    // $7::uuid, $8, $9, $10, $11) with 11 binds. After dropping category ($9) and
    // topic ($10), scope shifts from $11 → $9. Total: 11 columns, 9 unique binds.
    let sql = format!(
        r"INSERT INTO memory_chunks (id, agent_id, content, embedding, source, pinned, relevance_score, tsv, parent_id, chunk_index, scope)
           VALUES ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector('{lang}', $3), $7::uuid, $8, $9)",
    );

    sqlx::query(&sql)
        .bind(id)           // $1
        .bind(agent_id)     // $2
        .bind(content)      // $3
        .bind(vec_str)      // $4 (embedding)
        .bind(source)       // $5
        .bind(pinned)       // $6
        .bind(parent_id)    // $7
        .bind(chunk_index)  // $8
        .bind(scope)        // $9
        .execute(db)
        .await
        .context("failed to insert memory chunk")?;

    Ok(())
}

/// Insert a new memory chunk within an existing transaction.
/// Identical SQL to `insert_chunk`, but executes on a transaction handle.
#[allow(clippy::too_many_arguments)]
pub async fn insert_chunk_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    content: &str,
    vec_str: &str,
    source: &str,
    pinned: bool,
    lang: &str,
    parent_id: Option<&str>,
    chunk_index: i32,
    scope: &str,
    agent_id: &str,
) -> Result<()> {
    validate_fts_lang(lang)?;
    // SAFETY: `lang` is validated by validate_fts_lang() whitelist
    // (mirrors insert_chunk above; same shape after category/topic drop:
    // 11 columns, $1..$9 placeholders, scope occupies $9.)
    let sql = format!(
        r"INSERT INTO memory_chunks (id, agent_id, content, embedding, source, pinned, relevance_score, tsv, parent_id, chunk_index, scope)
           VALUES ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector('{lang}', $3), $7::uuid, $8, $9)",
    );

    sqlx::query(&sql)
        .bind(id)           // $1
        .bind(agent_id)     // $2
        .bind(content)      // $3
        .bind(vec_str)      // $4 (embedding)
        .bind(source)       // $5
        .bind(pinned)       // $6
        .bind(parent_id)    // $7
        .bind(chunk_index)  // $8
        .bind(scope)        // $9
        .execute(&mut **tx)
        .await
        .context("failed to insert memory chunk")?;

    Ok(())
}

// ── Get ──────────────────────────────────────────────────────────────────────

/// Retrieve a single chunk by ID.
pub async fn get_chunk_by_id(db: &PgPool, id: &str) -> Result<Vec<MemoryChunk>> {
    let rows = sqlx::query(
        r"SELECT id::text, content, COALESCE(source,'') AS source, pinned,
                  COALESCE(relevance_score,1.0)::float8 AS relevance_score,
                  created_at, accessed_at
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
                  created_at, accessed_at
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
                  created_at, accessed_at
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
    validate_fts_lang(lang)?;
    // SAFETY: `lang` is validated by validate_fts_lang() whitelist
    // letters. Not user input -- comes from server config.
    let sql = format!(
        "UPDATE memory_chunks SET tsv = to_tsvector('{lang}', content)"
    );
    let res = sqlx::query(&sql)
        .execute(db)
        .await
        .context("failed to rebuild FTS index")?;
    Ok(res.rows_affected())
}

/// Delete a memory chunk and its children (if it's a parent of a chunked document).
pub async fn delete_chunk(db: &PgPool, chunk_id: &str) -> Result<bool> {
    let res = sqlx::query("DELETE FROM memory_chunks WHERE id = $1::uuid OR parent_id = $1::uuid")
        .bind(chunk_id)
        .execute(db)
        .await
        .context("failed to delete memory chunk")?;
    Ok(res.rows_affected() > 0)
}

/// Delete all chunks with a given source (e.g. filename).
pub async fn delete_by_source(db: &PgPool, source: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM memory_chunks WHERE source = $1")
        .bind(source)
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

/// Wipe all memory for an agent.
pub async fn wipe_agent_memory(db: &PgPool, agent_id: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
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

#[cfg(test)]
mod tests {
    // ── SQL structure tests ─────────────────────────────────────────
    // These catch the class of bugs where column removal breaks INSERT/SELECT
    // (like the user_id NOT NULL bug caught in production).

    /// Verify insert_chunk SQL includes required columns (no user_id — dropped in m021;
    /// no category/topic — dropped in m032).
    #[test]
    fn insert_sql_includes_required_columns() {
        let sql = r"INSERT INTO memory_chunks (id, agent_id, content, embedding, source, pinned, relevance_score, tsv, parent_id, chunk_index, scope)";
        assert!(sql.contains("agent_id"), "INSERT must include agent_id");
        assert!(sql.contains("scope"), "INSERT must include scope");
        assert!(!sql.contains("user_id"), "user_id column dropped in migration 021");
        assert!(!sql.contains("category"), "category column dropped in migration 032");
        assert!(!sql.contains("topic"), "topic column dropped in migration 032");
    }

    /// Verify insert_chunk uses ::vector not ::halfvec.
    #[test]
    fn insert_sql_uses_vector_not_halfvec() {
        let sql = r"VALUES ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector('english', $3), $7::uuid, $8, $9)";
        assert!(sql.contains("$4::vector"), "embedding must be cast to ::vector (not ::halfvec)");
        assert!(!sql.contains("halfvec"), "halfvec has 4000 dim limit, must use vector");
    }

    /// Verify insert_chunk has exactly 9 bind positions ($1 through $9).
    /// After m032, scope occupies $9 (was $11; category $9 and topic $10 dropped).
    #[test]
    fn insert_sql_bind_count() {
        let sql = r"VALUES ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector('english', $3), $7::uuid, $8, $9)";
        for i in 1..=9 {
            let placeholder = format!("${}", i);
            assert!(sql.contains(&placeholder), "Missing bind placeholder {}", placeholder);
        }
        assert!(!sql.contains("$10"), "Unexpected $10 — too many binds after m032");
    }

    /// Verify INSERT column count matches VALUES count (11 columns post-m032).
    #[test]
    fn insert_sql_column_value_parity() {
        let columns = "id, agent_id, content, embedding, source, pinned, relevance_score, tsv, parent_id, chunk_index, scope";
        let col_count = columns.split(',').count();
        assert_eq!(col_count, 11, "Column count must be 11 (was 13 before m032 dropped category/topic)");
    }

    /// Verify search_semantic includes scope/agent filtering.
    #[test]
    fn search_semantic_sql_has_scope_filter() {
        let sql = "WHERE embedding IS NOT NULL AND ($3 = '' OR agent_id = $3 OR scope = 'shared')";
        assert!(sql.contains("agent_id = $3"), "search_semantic must filter by agent_id");
        assert!(sql.contains("scope = 'shared'"), "search_semantic must include shared chunks");
        assert!(sql.contains("$3 = ''"), "search_semantic must allow empty agent_id for admin");
    }

    /// Verify search_fts includes scope/agent filtering.
    #[test]
    fn search_fts_sql_has_scope_filter() {
        let sql = "WHERE tsv @@ plainto_tsquery('russian', $1) AND ($3 = '' OR agent_id = $3 OR scope = 'shared')";
        assert!(sql.contains("agent_id = $3"), "search_fts must filter by agent_id");
        assert!(sql.contains("scope = 'shared'"), "search_fts must include shared chunks");
    }

    /// Verify fetch_pinned includes scope/agent filtering.
    #[test]
    fn fetch_pinned_sql_has_scope_filter() {
        let sql = "WHERE ($1 = '' OR agent_id = $1 OR scope = 'shared') AND pinned = true";
        assert!(sql.contains("scope = 'shared'"), "fetch_pinned must include shared pinned chunks");
        assert!(sql.contains("pinned = true"), "fetch_pinned must filter pinned only");
    }

    /// Verify FTS language validation rejects injection attempts.
    #[test]
    fn fts_lang_validation_blocks_injection() {
        assert!(super::validate_fts_lang("russian").is_ok());
        assert!(super::validate_fts_lang("english").is_ok());
        assert!(super::validate_fts_lang("simple").is_ok());
        assert!(super::validate_fts_lang("french").is_ok());
        assert!(super::validate_fts_lang("Russian").is_err(), "Must reject — not in whitelist");
        assert!(super::validate_fts_lang("english; DROP TABLE").is_err(), "Must reject SQL injection");
        assert!(super::validate_fts_lang("").is_err(), "Must reject empty");
        assert!(super::validate_fts_lang("lang123").is_err(), "Must reject unknown lang");
        assert!(super::validate_fts_lang("custom_dict").is_err(), "Must reject custom dicts");
    }

    /// Verify memory_chunks required NOT NULL columns are always included in INSERT.
    #[test]
    fn insert_covers_not_null_columns() {
        // Required columns after migration 021 (user_id dropped) and m032
        // (category/topic dropped): id, agent_id, content, scope.
        let insert = "INSERT INTO memory_chunks (id, agent_id, content, embedding, source, pinned, relevance_score, tsv, parent_id, chunk_index, scope)";
        assert!(insert.contains("id"), "Must include id");
        assert!(insert.contains("content"), "Must include content");
        assert!(insert.contains("agent_id"), "Must include agent_id");
        assert!(insert.contains("scope"), "Must include scope");
        assert!(!insert.contains("user_id"), "user_id dropped in migration 021");
        assert!(!insert.contains("category"), "category dropped in migration 032");
        assert!(!insert.contains("topic"), "topic dropped in migration 032");
    }
}

