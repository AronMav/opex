use crate::tasks::MemoryTask;
use opex_embedding::ToolgateClient;
use sqlx::PgPool;
use serde_json::json;

pub async fn handle(
    task: &MemoryTask,
    db: &PgPool,
    client: &ToolgateClient,
    workspace_dir: &str,
    fts_language: &str,
) -> anyhow::Result<serde_json::Value> {
    let clear_existing = task.params["clear_existing"].as_bool().unwrap_or(false);
    let include_sessions = task.params["include_sessions"].as_bool().unwrap_or(true);
    let agent_id = task.params["agent_id"].as_str().unwrap_or("");

    let workspace_root = std::path::Path::new(workspace_dir);
    const EXCLUDE_DIRS: &[&str] = &["tools", "skills", "mcp", "uploads", "agents"];

    let md_files = collect_workspace_files(workspace_root, EXCLUDE_DIRS).await?;

    // Audit 2026-05-08 (5th pass): the DELETE used to run BEFORE any new
    // chunk was inserted. A worker crash between the DELETE and the
    // first successful embed_and_insert wiped the agent's memory until
    // the next reindex ran. We now record the timestamp BEFORE inserts
    // begin and run the DELETE only AFTER every chunk we intend to keep
    // is in the table. Worst-case window is duplicate rows during the
    // indexing window (cleaned up by the trailing DELETE) instead of an
    // empty memory window.
    //
    // 6th pass: `reindex_started` is read FROM POSTGRES, not from the
    // worker process clock. `memory_chunks.created_at` is filled by
    // PostgreSQL's `DEFAULT now()`; comparing two timestamps from the same
    // clock source eliminates a clock-skew window where a fresh chunk
    // could get a `created_at < reindex_started` (NTP rebound, virtualised
    // hosts) and be silently deleted by the trailing cleanup.
    let reindex_started: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT now()")
            .fetch_one(db)
            .await?;

    let total_files = md_files.len();
    let mut indexed = 0u32;
    let mut errors = 0u32;

    // Index workspace files
    for path in &md_files {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) if c.len() > 50 => c,
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(path = ?path, error = %e, "failed to read");
                errors += 1;
                continue;
            }
        };
        let source = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        match embed_and_insert(db, client, &content, &source, fts_language, agent_id).await {
            Ok(()) => indexed += 1,
            Err(e) => {
                tracing::warn!(source = %source, error = %e, "index failed");
                errors += 1;
            }
        }

        if indexed.is_multiple_of(50) && indexed > 0 {
            tracing::info!(indexed, total_files, "reindex progress");
            // Ping systemd watchdog to prevent timeout during long reindex
            #[cfg(target_os = "linux")]
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
        }
    }

    // Index session transcripts
    let mut session_indexed = 0u32;
    if include_sessions && !agent_id.is_empty() {
        // F117: propagate a real DB failure so the reindex task fails (and is
        // retried) instead of silently under-indexing sessions while reporting
        // success. Still logs before bubbling up.
        session_indexed = index_sessions(db, client, fts_language, agent_id)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, "session transcript indexing failed"))?;
    }

    tracing::info!(indexed, errors, total_files, session_indexed, "universal reindex complete");
    // Treat "every file failed" as a task failure so the worker auto-retry
    // mechanism picks it up (catches transient toolgate-down races where the
    // first attempt fires before /v1/embeddings is ready). Partial successes
    // stay 'done' — operator can re-trigger if they want full coverage.
    if total_files > 0 && indexed == 0 && errors > 0 {
        anyhow::bail!(
            "reindex failed: 0 files indexed, {errors} errors out of {total_files} (likely transient embedding error)"
        );
    }

    // Trailing DELETE: drop the OLD chunks (created before reindex_started)
    // now that the new ones are committed. If the worker crashed earlier in
    // this function, this line never runs, the agent keeps its old memory,
    // and the next reindex picks up where this one left off.
    //
    // 6th pass: gate on `(indexed + session_indexed) > 0` (was just
    // `indexed`). Workspace-only reindex with `include_sessions=false` and
    // `total_files=0` would otherwise leave every old chunk in place; vice
    // versa, a session-only reindex (`include_sessions=true` with no
    // workspace files to index) used to skip the cleanup entirely.
    if clear_existing && !agent_id.is_empty() && (indexed + session_indexed) > 0 {
        let removed = delete_pre_reindex_chunks(db, agent_id, reindex_started).await?;
        tracing::info!(
            agent_id,
            removed,
            "removed pre-reindex chunks after successful re-population",
        );
    }

    Ok(json!({
        "indexed": indexed,
        "session_indexed": session_indexed,
        "errors": errors,
        "total_files": total_files,
    }))
}

/// kind='fact' guard: reindex re-populates FILE-backed chunks only; soul
/// biography (event/reflection) must survive clear_existing (spec §1, rev3 blocker).
pub(crate) async fn delete_pre_reindex_chunks(
    db: &sqlx::PgPool,
    agent_id: &str,
    cutoff: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<u64> {
    let cleared = sqlx::query(
        "DELETE FROM memory_chunks \
         WHERE agent_id = $1 \
           AND created_at < $2 \
           AND kind = 'fact'",
    )
    .bind(agent_id)
    .bind(cutoff)
    .execute(db)
    .await?;
    Ok(cleared.rows_affected())
}

/// Collect all .md and .txt files from `workspace_root`, skipping excluded top-level dirs.
pub(crate) async fn collect_workspace_files(
    workspace_root: &std::path::Path,
    exclude_dirs: &[&str],
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![workspace_root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('.') || name.starts_with('_') {
                    continue;
                }
                // Only exclude at top-level
                let is_top_level = path.parent() == Some(workspace_root);
                if is_top_level && exclude_dirs.contains(&name) {
                    continue;
                }
                stack.push(path);
            } else {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // Composite-suffix exclude (e.g. `.excalidraw.md` — Excalidraw
                // drawings stored as Markdown with embedded JSON/PNG, bloated
                // and meaningless for semantic search). Keep in sync with
                // `opex_core::agent::workspace::MEMORY_INDEX_EXCLUDE_SUFFIXES`.
                const EXCLUDE_SUFFIXES: &[&str] = &[".excalidraw.md"];
                let lower = name.to_ascii_lowercase();
                if EXCLUDE_SUFFIXES.iter().any(|sfx| lower.ends_with(sfx)) {
                    continue;
                }
                if lower.ends_with(".md") || lower.ends_with(".txt") {
                    files.push(path);
                }
            }
        }
    }
    Ok(files)
}

/// Index session transcripts from DB into `memory_chunks`.
async fn index_sessions(
    db: &PgPool,
    client: &ToolgateClient,
    fts_language: &str,
    agent_id: &str,
) -> anyhow::Result<u32> {
    // IMPORTANT: sessions table uses started_at, not created_at
    let sessions: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM sessions WHERE agent_id = $1 \
         AND started_at > now() - interval '90 days' ORDER BY started_at DESC",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;

    let mut indexed = 0u32;
    for (session_id,) in &sessions {
        let source = format!("session:{session_id}");

        // F117: propagate a real DB error instead of coercing it to an empty Vec.
        // Swallowing it under-indexed the session silently while the task still
        // reported success; `?` marks the reindex failed so it is retried.
        let messages: Vec<(String, String)> = sqlx::query_as(
            "SELECT role, content FROM messages WHERE session_id = $1 \
             AND role IN ('user', 'assistant') AND length(content) > 10 \
             ORDER BY created_at ASC",
        )
        .bind(session_id)
        .fetch_all(db)
        .await?;

        if messages.is_empty() {
            continue;
        }

        let transcript: String = messages
            .iter()
            .map(|(role, content)| format!("[{role}]: {content}"))
            .collect::<Vec<_>>()
            .join("\n\n");

        if transcript.len() < 100 {
            continue;
        }

        match embed_and_insert(db, client, &transcript, &source, fts_language, agent_id).await {
            Ok(()) => indexed += 1,
            Err(e) => tracing::debug!(session = %session_id, error = %e, "session index failed"),
        }
    }
    Ok(indexed)
}

/// Embed content and insert into `memory_chunks` (transactional replace).
async fn embed_and_insert(
    db: &PgPool,
    client: &ToolgateClient,
    content: &str,
    source: &str,
    fts_language: &str,
    agent_id: &str,
) -> anyhow::Result<()> {
    // Embed via shared ToolgateClient (retry policy + tracing applied automatically).
    let emb = client.embed_one(content).await?;

    let vec_str = format!(
        "[{}]",
        emb.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join(",")
    );

    // Transaction: delete old + insert new.
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM memory_chunks WHERE source = $1")
        .bind(source)
        .execute(&mut *tx)
        .await?;

    let id = uuid::Uuid::new_v4().to_string();
    // fts_language is configurable per-deployment (e.g. 'russian', 'english') instead of hardcoded
    let insert_sql = format!(
        "INSERT INTO memory_chunks (id, content, embedding, source, pinned, relevance_score, tsv, agent_id, scope)
         VALUES ($1::uuid, $2, $3::halfvec, $4, false, 1.0, to_tsvector('{fts_language}', $2), $5, 'shared')"
    );
    sqlx::query(&insert_sql)
        .bind(&id)        // $1
        .bind(content)    // $2
        .bind(&vec_str)   // $3 (embedding)
        .bind(source)     // $4
        .bind(agent_id)   // $5
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collect_skips_excluded_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let tools_dir = tmp.path().join("tools");
        let notes_dir = tmp.path().join("notes");
        tokio::fs::create_dir_all(&tools_dir).await.unwrap();
        tokio::fs::create_dir_all(&notes_dir).await.unwrap();
        tokio::fs::write(tools_dir.join("tool.md"), "tool").await.unwrap();
        tokio::fs::write(notes_dir.join("note.md"), "note").await.unwrap();

        let exclude = &["tools", "skills", "mcp", "uploads", "agents"];
        let files = collect_workspace_files(tmp.path(), exclude).await.unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].to_string_lossy().contains("notes"));
    }

    #[tokio::test]
    async fn collect_finds_md_and_txt() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::write(tmp.path().join("a.md"), "a").await.unwrap();
        tokio::fs::write(tmp.path().join("b.txt"), "b").await.unwrap();
        tokio::fs::write(tmp.path().join("c.rs"), "c").await.unwrap();

        let files = collect_workspace_files(tmp.path(), &[]).await.unwrap();
        assert_eq!(files.len(), 2);
    }

}

#[cfg(test)]
mod soul_guard_tests {
    #[sqlx::test(migrations = "../../migrations")]
    async fn reindex_clear_existing_spares_soul_kinds(db: sqlx::PgPool) {
        // one 'fact' and one 'event' chunk, both older than the cutoff
        for (kind, id) in [("fact", "a"), ("event", "b")] {
            sqlx::query(
                "INSERT INTO memory_chunks (id, agent_id, content, source, pinned, scope, kind, created_at) \
                 VALUES (gen_random_uuid(), 'A', $1, 'soul_event:s', false, 'private', $2, now() - interval '1 hour')",
            )
            .bind(format!("content-{id}"))
            .bind(kind)
            .execute(&db).await.unwrap();
        }
        // Через ПРОДОВУЮ функцию, не копию SQL (ревью плана):
        let removed = super::delete_pre_reindex_chunks(&db, "A", chrono::Utc::now()).await.unwrap();
        assert_eq!(removed, 1);

        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM memory_chunks WHERE agent_id = 'A'")
            .fetch_all(&db).await.unwrap();
        assert_eq!(kinds, vec!["event".to_string()], "event must survive, fact must be deleted");
    }
}
