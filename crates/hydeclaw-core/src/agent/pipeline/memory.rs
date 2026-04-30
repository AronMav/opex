//! Pipeline step: memory — memory augmentation and knowledge extraction (migrated from engine_memory.rs).
//!
//! All functions take explicit dependencies instead of `&self` on `AgentEngine`.

use crate::agent::memory_service::MemoryService;

pub(crate) const MEMORY_CHUNK_MAX_CHARS: usize = 6_000;

/// Truncate a memory chunk to fit context budget.
///
/// Excalidraw docs are replaced with a short placeholder; other content
/// is hard-capped at `MEMORY_CHUNK_MAX_CHARS` by Unicode scalar boundary.
pub(crate) fn truncate_chunk_content(content: &str) -> &str {
    if content.contains("excalidraw-plugin: parsed")
        || content.contains("== EXCALIDRAW VIEW ==")
    {
        return "[Excalidraw diagram — binary content, skipped]";
    }
    let limit = content.floor_char_boundary(MEMORY_CHUNK_MAX_CHARS.min(content.len()));
    &content[..limit]
}

// ── MemoryContext ────────────────────────────────────────────────────────────

/// Result of L0 pinned chunk loading.
pub(crate) struct MemoryContext {
    /// Formatted text to append to system prompt (empty if no pinned chunks).
    pub pinned_text: String,
    /// IDs of pinned chunks already loaded (for L2 dedup).
    pub pinned_ids: Vec<String>,
}

// ── Build L0 memory context ─────────────────────────────────────────────────

/// Build L0 memory context: load pinned chunks for the given agent.
pub async fn build_memory_context(
    memory_store: &dyn MemoryService,
    agent_name: &str,
    budget_tokens: u32,
) -> MemoryContext {
    if !memory_store.is_available() {
        return MemoryContext { pinned_text: String::new(), pinned_ids: vec![] };
    }
    match memory_store.load_pinned(agent_name, budget_tokens).await {
        Ok((text, ids)) => MemoryContext { pinned_text: text, pinned_ids: ids },
        Err(e) => {
            tracing::warn!(error = %e, "failed to load pinned memory chunks");
            MemoryContext { pinned_text: String::new(), pinned_ids: vec![] }
        }
    }
}

// ── Index extracted facts ───────────────────────────────────────────────────

/// Index extracted facts into memory (called after session compaction via /compact).
/// Uses batch embedding for efficiency when multiple facts are available.
pub async fn index_facts_to_memory(
    memory_store: &dyn MemoryService,
    agent_name: &str,
    facts: &[String],
) {
    if !memory_store.is_available() {
        return;
    }
    let items: Vec<(String, String, bool, String)> = facts
        .iter()
        .filter(|f| !f.trim().is_empty())
        .map(|f| (f.clone(), "extracted".to_string(), false, "shared".to_string()))
        .collect();
    if items.is_empty() {
        return;
    }
    match memory_store.index_batch(&items, agent_name).await {
        Ok(ids) => tracing::info!(count = ids.len(), "batch indexed facts to memory"),
        Err(e) => {
            tracing::warn!(error = %e, "batch index failed, falling back to individual inserts");
            let mut ok = 0usize;
            let mut fail = 0usize;
            for (content, source, pinned, scope) in &items {
                match memory_store.index(content, source, *pinned, scope, agent_name).await {
                    Ok(_) => ok += 1,
                    Err(ie) => {
                        fail += 1;
                        tracing::warn!(error = %ie, "individual fact index failed");
                    }
                }
            }
            tracing::info!(ok, fail, "individual fact indexing complete");
        }
    }
}

// ── Tool handlers ───────────────────────────────────────────────────────────

/// Internal tool: search long-term memory.
pub async fn handle_memory_search(
    memory_store: &dyn MemoryService,
    agent_name: &str,
    pinned_ids: &[String],
    args: &serde_json::Value,
) -> String {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    if query.is_empty() {
        return "Error: 'query' is required".to_string();
    }

    // Search long-term memory (exclude L0 pinned chunks to avoid duplication)
    match memory_store.search(query, limit, pinned_ids, agent_name).await {
        Ok((results, _)) if results.is_empty() => {
            "No relevant memories found.".to_string()
        }
        Ok((results, mode)) => {
            let header = if mode == "fts" { "[FTS fallback] " } else { "" };
            let body = results
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let pin = if r.pinned { "\u{1f4cc} " } else { "" };
                    format!("{}. [{}] {}{}  (id: {})", i + 1, r.source, pin, r.content, r.id)
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}[Memory]\n{}", header, body)
        }
        Err(e) => format!("Memory search error: {}", e),
    }
}

/// Internal tool: index content into long-term memory.
pub async fn handle_memory_index(
    memory_store: &dyn MemoryService,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("manual");
    let pinned = args.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false);
    let scope = match args.get("shared").and_then(|v| v.as_bool()).unwrap_or(false) {
        true => "shared",
        false => "private",
    };

    if content.is_empty() {
        return "Error: 'content' is required".to_string();
    }
    if !memory_store.is_available() {
        return "Memory indexing is not available (embedding endpoint not configured).".to_string();
    }

    match memory_store.index(content, source, pinned, scope, agent_name).await {
        Ok(id) => format!("Indexed as {}", id),
        Err(e) => format!("Memory index error: {}", e),
    }
}

/// Internal tool: bulk re-index all .md/.txt files from the entire workspace into memory.
/// Scans the whole workspace (excluding system dirs). Returns immediately — worker processes async.
pub async fn handle_memory_reindex(
    memory_store: &dyn MemoryService,
    agent_name: &str,
    workspace_dir: &str,
    args: &serde_json::Value,
) -> String {
    let clear_existing = args.get("clear_existing").and_then(|v| v.as_bool()).unwrap_or(false);
    let include_sessions = args.get("include_sessions").and_then(|v| v.as_bool()).unwrap_or(true);

    if !memory_store.is_available() {
        return "Memory indexing is not available (embedding endpoint not configured).".to_string();
    }

    let workspace_root = std::path::PathBuf::from(workspace_dir);
    if !workspace_root.exists() {
        return "Workspace directory not found.".to_string();
    }

    // Count indexable files for user feedback (entire workspace, skip system dirs)
    let mut file_count = 0usize;
    let exclude_dirs = crate::agent::workspace::MEMORY_INDEX_EXCLUDE_DIRS;
    let mut stack = vec![workspace_root.clone()];
    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let rel = path.strip_prefix(&workspace_root).ok()
                    .and_then(|p| p.components().next())
                    .and_then(|c| c.as_os_str().to_str())
                    .unwrap_or("");
                if !name.starts_with('.') && !exclude_dirs.contains(&rel) {
                    stack.push(path);
                }
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("md") | Some("txt")) {
                file_count += 1;
            }
        }
    }

    // Clear existing memory synchronously (fast DB operation)
    if clear_existing {
        match memory_store.wipe_agent_memory(agent_name).await {
            Ok(deleted) => tracing::info!(deleted, agent = %agent_name, "cleared memory before reindex"),
            Err(e) => return format!("Failed to clear memory: {}", e),
        }
    }

    // Create reindex task for memory-worker
    let task_id = match memory_store.enqueue_reindex_task(serde_json::json!({
        "clear_existing": clear_existing,
        "include_sessions": include_sessions,
        "agent_id": agent_name,
    })).await {
        Ok(id) => id,
        Err(e) => return format!("Failed to create reindex task: {}", e),
    };

    format!(
        "Reindex task created: ~{} indexable files in workspace{}. Task ID: {}. Worker will process.",
        file_count,
        if include_sessions { " + session transcripts" } else { "" },
        task_id
    )
}

/// Internal tool: get memory chunks by ID or source.
pub async fn handle_memory_get(
    memory_store: &dyn MemoryService,
    args: &serde_json::Value,
) -> String {
    let chunk_id = args.get("chunk_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let source = args.get("source").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    match memory_store.get(chunk_id, source, limit).await {
        Ok(chunks) if chunks.is_empty() => "No memory chunks found.".to_string(),
        Ok(chunks) => chunks
            .iter()
            .map(|c| {
                let pin = if c.pinned { "\u{1f4cc} " } else { "" };
                format!(
                    "[{}] {}(score:{:.2}) {}\n  id: {} | created: {}",
                    c.source, pin, c.relevance_score, c.content,
                    c.id, c.created_at.format("%Y-%m-%d %H:%M")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        Err(e) => format!("Memory get error: {}", e),
    }
}

/// Internal tool: delete a memory chunk by UUID.
pub async fn handle_memory_delete(
    memory_store: &dyn MemoryService,
    args: &serde_json::Value,
) -> String {
    let chunk_id = match args.get("chunk_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => return "Error: 'chunk_id' is required".to_string(),
    };

    match memory_store.delete(chunk_id).await {
        Ok(true) => format!("Deleted memory chunk {}", chunk_id),
        Ok(false) => format!("Memory chunk {} not found", chunk_id),
        Err(e) => format!("Error deleting memory chunk: {}", e),
    }
}

/// Internal tool: add/update/remove an entry in the agent's MEMORY.md file.
pub async fn handle_memory_update(
    memory_md_lock: &tokio::sync::Mutex<()>,
    workspace_dir: &str,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let section = match args.get("section").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return "Error: 'section' is required".to_string(),
    };
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("add");
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => return "Error: 'content' is required".to_string(),
    };

    // Atomic read-modify-write: hold lock for the entire operation
    let _lock = memory_md_lock.lock().await;

    let memory_path = std::path::Path::new(workspace_dir)
        .join("agents")
        .join(agent_name)
        .join("MEMORY.md");

    let existing = tokio::fs::read_to_string(&memory_path).await.unwrap_or_default();

    let updated = match action {
        "add" => {
            let section_header = format!("# {}", section);
            if existing.contains(&section_header) {
                existing.replacen(
                    &section_header,
                    &format!("{}\n- {}", section_header, content),
                    1,
                )
            } else {
                format!("{}\n# {}\n- {}\n", existing.trim_end(), section, content)
            }
        }
        "update" => {
            let lines: Vec<String> = existing
                .lines()
                .map(|l| {
                    let key = content.split(':').next().unwrap_or(&content).trim();
                    if l.starts_with("- ") && l.contains(key) {
                        format!("- {}", content)
                    } else {
                        l.to_string()
                    }
                })
                .collect();
            lines.join("\n")
        }
        "remove" => {
            let lines: Vec<&str> = existing
                .lines()
                .filter(|l| !l.contains(&content))
                .collect();
            lines.join("\n")
        }
        _ => return format!("Unknown action '{}'. Use: add, update, remove", action),
    };

    // Guard against unbounded growth
    const MAX_MEMORY_MD_BYTES: usize = 8 * 1024;
    if updated.len() > MAX_MEMORY_MD_BYTES {
        return format!(
            "Error: MEMORY.md would exceed {} KB limit ({} KB). Remove old entries first or use memory_index for large data.",
            MAX_MEMORY_MD_BYTES / 1024,
            updated.len() / 1024
        );
    }

    match tokio::fs::write(&memory_path, &updated).await {
        Ok(_) => format!(
            "MEMORY.md updated ({} in section '{}'):\n- {}",
            action, section, content
        ),
        Err(e) => format!("Error writing MEMORY.md: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_chunk_content;

    #[test]
    fn excalidraw_marker_replaced() {
        let big = format!("excalidraw-plugin: parsed\n{}", "x".repeat(100_000));
        let out = truncate_chunk_content(&big);
        assert_eq!(out, "[Excalidraw diagram — binary content, skipped]");
    }

    #[test]
    fn excalidraw_view_marker_replaced() {
        let big = "== EXCALIDRAW VIEW ==\nsome data";
        let out = truncate_chunk_content(big);
        assert_eq!(out, "[Excalidraw diagram — binary content, skipped]");
    }

    #[test]
    fn long_text_truncated_to_limit() {
        let long = "a".repeat(10_000);
        let out = truncate_chunk_content(&long);
        assert_eq!(out.len(), super::MEMORY_CHUNK_MAX_CHARS);
    }

    #[test]
    fn short_text_unchanged() {
        let short = "hello world";
        assert_eq!(truncate_chunk_content(short), short);
    }

    #[test]
    fn exactly_at_limit_unchanged() {
        let at_limit = "b".repeat(super::MEMORY_CHUNK_MAX_CHARS);
        let out = truncate_chunk_content(&at_limit);
        assert_eq!(out.len(), super::MEMORY_CHUNK_MAX_CHARS);
    }
}
