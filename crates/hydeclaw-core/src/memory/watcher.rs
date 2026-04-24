/// Watch workspace directory for .md/.txt file changes and auto-index into memory.
/// Uses timer-based debounce: waits for 5s of quiet after last change before re-indexing.
use crate::memory::MemoryStore;

pub fn spawn_workspace_watcher(
    workspace_dir: String,
    memory: std::sync::Arc<MemoryStore>,
    handle: tokio::runtime::Handle,
) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "workspace watcher failed to start");
                return;
            }
        };

        // Watch entire workspace root -- exclude system dirs at event time.
        // Canonicalize so the exclude check below can strip a consistent
        // prefix: `notify` emits absolute paths, and `workspace_dir` is
        // typically a relative literal ("workspace") from config. Without
        // canonicalization, `strip_prefix` silently fails and EVERY file
        // passes the exclude check (skills/, agents/*/SOUL.md, etc.).
        let watch_dir_path = dunce::canonicalize(&workspace_dir)
            .unwrap_or_else(|_| std::path::PathBuf::from(&workspace_dir));
        let watch_dir = watch_dir_path.as_path();

        if let Err(e) = watcher.watch(watch_dir, RecursiveMode::Recursive) {
            tracing::error!(error = %e, path = ?watch_dir, "failed to watch workspace dir");
            return;
        }

        tracing::info!(dir = ?watch_dir, "workspace file watcher started");

        let mut pending_files: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
        let mut debounce_deadline: Option<std::time::Instant> = None;

        loop {
            let timeout = debounce_deadline
                .map_or(std::time::Duration::from_secs(3600), |d| d.saturating_duration_since(std::time::Instant::now()));

            match rx.recv_timeout(timeout) {
                Ok(Ok(Event { kind: EventKind::Create(_) | EventKind::Modify(_), paths, .. })) => {
                    let exclude_dirs = crate::agent::workspace::MEMORY_INDEX_EXCLUDE_DIRS;
                    let exclude_files = crate::agent::workspace::MEMORY_INDEX_EXCLUDE_FILES;
                    for p in paths {
                        // Skip files in system directories. Check ANY path
                        // component — belt-and-suspenders over strip_prefix
                        // against the canonical watch_dir, so a nested file
                        // like `workspace/agents/Hyde/SOUL.md` is caught even
                        // if the prefix comparison fails.
                        let in_excluded_dir = p.strip_prefix(&watch_dir_path)
                            .ok()
                            .and_then(|rel| rel.components().next())
                            .and_then(|c| c.as_os_str().to_str())
                            .is_some_and(|first| exclude_dirs.contains(&first))
                            || p.components().any(|c| {
                                c.as_os_str()
                                    .to_str()
                                    .is_some_and(|s| exclude_dirs.contains(&s))
                            });
                        if in_excluded_dir {
                            continue;
                        }
                        // Skip root-level system docs (AGENTS.md, TOOLS.md,
                        // AUTHORITY.md) — these are governance / reference
                        // docs, not user knowledge.
                        let is_excluded_file = p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|name| exclude_files.contains(&name));
                        if is_excluded_file {
                            continue;
                        }
                        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                        if matches!(ext, "md" | "txt") {
                            pending_files.insert(p);
                        }
                    }
                    if !pending_files.is_empty() {
                        debounce_deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                    }
                }
                Ok(_) => {} // other events
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Debounce fired -- process pending files
                    if !pending_files.is_empty() {
                        let files: Vec<std::path::PathBuf> = pending_files.drain().collect();
                        let mem = memory.clone();
                        let workspace_dir_clone = workspace_dir.clone();
                        handle.spawn(async move {
                            // Try first file to check if embedding is reachable
                            let mut indexed = 0u32;
                            for path in &files {
                                let content = match tokio::fs::read_to_string(path).await {
                                    Ok(c) if c.len() > 50 => c, // skip tiny files
                                    _ => continue,
                                };
                                let workspace_root = std::path::Path::new(&workspace_dir_clone);
                                let source = path.strip_prefix(workspace_root)
                                    .unwrap_or(path.as_path())
                                    .to_string_lossy()
                                    .to_string();
                                // Delete existing chunks from this source, then re-index
                                if let Err(e) = mem.delete_by_source(&source).await {
                                    tracing::debug!(source = %source, error = %e, "no existing chunks to delete");
                                }
                                match mem.index(&content, &source, false, None, None, "shared", "").await {
                                    Ok(_) => indexed += 1,
                                    Err(e) => {
                                        tracing::debug!(error = %e, "embedding unavailable -- skipping workspace indexing");
                                        break;
                                    }
                                }
                            }
                            if indexed > 0 {
                                tracing::info!(count = indexed, "workspace watcher: re-indexed changed files");
                            }
                        });
                        debounce_deadline = None;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}
