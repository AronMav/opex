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
                  (1.0 - (embedding <=> $1::vector))::float8 AS similarity
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

async fn search_fts_inner(
    db: &PgPool,
    query: &str,
    limit: i64,
    lang: &str,
    agent_id: &str,
    or_mode: bool,
) -> Result<Vec<MemoryResult>> {
    validate_fts_lang(lang)?;
    // SAFETY: `lang` is validated by validate_fts_lang() whitelist
    // letters. Not user input -- comes from server config.
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
                  ts_rank_cd(tsv, {tsquery_fn}('{lang}', $1))::float8 AS similarity
           FROM memory_chunks
           WHERE tsv @@ {tsquery_fn}('{lang}', $1)
             AND ($3 = '' OR agent_id = $3 OR scope = 'shared')
           ORDER BY ts_rank_cd(tsv, {tsquery_fn}('{lang}', $1)) DESC,
                    relevance_score DESC
           LIMIT $2",
    );

    let rows = sqlx::query(&sql)
        .bind(&effective_query)
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
) -> Result<()> {
    validate_fts_lang(lang)?;
    // SAFETY: `lang` is validated by validate_fts_lang() whitelist
    // letters. Not user input -- comes from server config.
    //
    // History:
    //   m032 dropped category ($9) and topic ($10) — scope shifted $11 → $9
    //   m033 dropped parent_id ($7) and chunk_index ($8) — scope shifted $9 → $7
    // Current: 9 columns, 7 unique binds, scope at $7.
    let sql = format!(
        r"INSERT INTO memory_chunks (id, agent_id, content, embedding, source, pinned, relevance_score, tsv, scope)
           VALUES ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector('{lang}', $3), $7)",
    );

    sqlx::query(&sql)
        .bind(id)           // $1
        .bind(agent_id)     // $2
        .bind(content)      // $3
        .bind(vec_str)      // $4 (embedding)
        .bind(source)       // $5
        .bind(pinned)       // $6
        .bind(scope)        // $7
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
    scope: &str,
    agent_id: &str,
) -> Result<()> {
    validate_fts_lang(lang)?;
    // SAFETY: `lang` is validated by validate_fts_lang() whitelist.
    // Mirrors insert_chunk above; same shape after m032 + m033 drops:
    // 9 columns, $1..$7 placeholders, scope occupies $7.
    let sql = format!(
        r"INSERT INTO memory_chunks (id, agent_id, content, embedding, source, pinned, relevance_score, tsv, scope)
           VALUES ($1::uuid, $2, $3, $4::vector, $5, $6, 1.0, to_tsvector('{lang}', $3), $7)",
    );

    sqlx::query(&sql)
        .bind(id)           // $1
        .bind(agent_id)     // $2
        .bind(content)      // $3
        .bind(vec_str)      // $4 (embedding)
        .bind(source)       // $5
        .bind(pinned)       // $6
        .bind(scope)        // $7
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

    #[test]
    fn or_tsquery_embedded_operators_within_word_are_stripped() {
        // "foo&bar" → split_whitespace → ["foo&bar"] → strip '&' → "foobar" → "foobar"
        // Distinct from "foo & bar" where '&' is a separate token
        assert_eq!(super::or_tsquery_from("foo&bar"), "foobar");
    }}

