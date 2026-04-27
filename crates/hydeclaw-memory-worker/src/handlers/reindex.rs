use crate::tasks::MemoryTask;
use sqlx::PgPool;
use serde_json::json;

pub async fn handle(
    task: &MemoryTask,
    db: &PgPool,
    toolgate_url: &str,
    workspace_dir: &str,
    fts_language: &str,
) -> anyhow::Result<serde_json::Value> {
    let clear_existing = task.params["clear_existing"].as_bool().unwrap_or(false);
    let include_sessions = task.params["include_sessions"].as_bool().unwrap_or(true);
    let agent_id = task.params["agent_id"].as_str().unwrap_or("");

    // Legacy compat: if "directory" field present, use old path-specific behavior
    if let Some(dir) = task.params["directory"].as_str() {
        return handle_legacy_directory(task, db, toolgate_url, workspace_dir, fts_language, dir).await;
    }

    let workspace_root = std::path::Path::new(workspace_dir);
    const EXCLUDE_DIRS: &[&str] = &["tools", "skills", "mcp", "uploads", "agents"];

    let md_files = collect_workspace_files(workspace_root, EXCLUDE_DIRS).await?;

    // Clear existing (scoped by agent_id)
    if clear_existing && !agent_id.is_empty() {
        sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
            .bind(agent_id)
            .execute(db)
            .await?;
        tracing::info!(agent_id, "cleared memory before universal reindex");
    }

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

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

        match embed_and_insert(db, &http, toolgate_url, &content, &source, fts_language, agent_id).await {
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
        session_indexed = index_sessions(db, &http, toolgate_url, fts_language, agent_id)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "session transcript indexing failed");
                0
            });
    }

    tracing::info!(indexed, errors, total_files, session_indexed, "universal reindex complete");
    Ok(json!({
        "indexed": indexed,
        "session_indexed": session_indexed,
        "errors": errors,
        "total_files": total_files,
    }))
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
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if matches!(ext, "md" | "txt") {
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
    http: &reqwest::Client,
    toolgate_url: &str,
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

        let messages: Vec<(String, String)> = sqlx::query_as(
            "SELECT role, content FROM messages WHERE session_id = $1 \
             AND role IN ('user', 'assistant') AND length(content) > 10 \
             ORDER BY created_at ASC",
        )
        .bind(session_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();

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

        match embed_and_insert(db, http, toolgate_url, &transcript, &source, fts_language, agent_id).await {
            Ok(()) => indexed += 1,
            Err(e) => tracing::debug!(session = %session_id, error = %e, "session index failed"),
        }
    }
    Ok(indexed)
}

/// Legacy handler for tasks that still carry a "directory" field.
async fn handle_legacy_directory(
    task: &MemoryTask,
    db: &PgPool,
    toolgate_url: &str,
    workspace_dir: &str,
    fts_language: &str,
    directory: &str,
) -> anyhow::Result<serde_json::Value> {
    let clear_existing = task.params["clear_existing"].as_bool().unwrap_or(false);
    let agent_id = task.params["agent_id"].as_str().unwrap_or("");

    // System directories must never be indexed — their contents (skills,
    // tools, MCP configs, agent identity files) are not user knowledge and
    // would poison long-term memory with prompt fragments.
    const SYSTEM_DIRS: &[&str] = &["tools", "skills", "mcp", "uploads", "agents"];
    if SYSTEM_DIRS.contains(&directory) {
        anyhow::bail!("refusing to index system directory '{directory}'");
    }

    let base = std::path::PathBuf::from(workspace_dir).join(directory);
    if !base.exists() || !base.is_dir() {
        anyhow::bail!("directory '{directory}' not found");
    }

    // Collect .md files
    let mut md_files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !name.starts_with('.') && !name.starts_with('_') {
                    stack.push(path);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                md_files.push(path);
            }
        }
    }

    if md_files.is_empty() {
        return Ok(json!({"indexed": 0, "total": 0}));
    }

    // Clear existing (scoped by agent_id)
    if clear_existing {
        if agent_id.is_empty() {
            sqlx::query("DELETE FROM memory_chunks").execute(db).await?;
        } else {
            sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1")
                .bind(agent_id)
                .execute(db)
                .await?;
        }
        tracing::info!(agent_id, "cleared memory data (legacy reindex)");
    }

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let total = md_files.len();
    let mut indexed = 0u32;
    let mut errors = 0u32;

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
            .strip_prefix(&base)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        match embed_and_insert(db, &http, toolgate_url, &content, &source, fts_language, agent_id).await {
            Ok(()) => indexed += 1,
            Err(e) => {
                tracing::warn!(source = %source, error = %e, "index failed");
                errors += 1;
            }
        }

        if indexed.is_multiple_of(50) && indexed > 0 {
            tracing::info!(indexed, total, "reindex progress");
            #[cfg(target_os = "linux")]
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
        }
    }

    tracing::info!(indexed, errors, total, "legacy reindex complete");
    Ok(json!({"indexed": indexed, "errors": errors, "total": total}))
}

/// Embed content and insert into `memory_chunks` (transactional replace).
async fn embed_and_insert(
    db: &PgPool,
    http: &reqwest::Client,
    toolgate_url: &str,
    content: &str,
    source: &str,
    fts_language: &str,
    agent_id: &str,
) -> anyhow::Result<()> {
    // Embed via toolgate (single call — chunking removed in m033)
    let body = serde_json::json!({"input": [content]});
    let resp = http
        .post(format!("{}/v1/embeddings", toolgate_url.trim_end_matches('/')))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("embedding API error {status}: {text}");
    }

    let resp_body: serde_json::Value = resp.json().await?;
    let emb: Vec<f32> = resp_body["data"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|d| d["embedding"].as_array())
        .map(|a| a.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect())
        .ok_or_else(|| anyhow::anyhow!("missing embedding in response"))?;

    let vec_str = format!(
        "[{}]",
        emb.iter().map(std::string::ToString::to_string).collect::<Vec<_>>().join(",")
    );

    // Transaction: delete old + insert new.
    // History (m033): WAS 11 columns including parent_id ($5) and chunk_index ($6),
    // INSERT ran in a loop once per chunk. After m033: 9 columns, agent_id at $5,
    // single INSERT, no chunking.
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
