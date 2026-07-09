/// Watch workspace directory for .md/.txt file changes and auto-index into memory.
/// Uses timer-based debounce: waits for 5s of quiet after last change before re-indexing.
use crate::memory::MemoryStore;

/// Spawns the workspace file watcher in a background OS thread.
///
/// The thread polls `cancel.is_cancelled()` every 250 ms (via `recv_timeout`) so
/// it exits promptly on graceful shutdown instead of blocking the process (Bug 12).
///
/// In-flight per-file index tasks are tracked with a `JoinSet`; on shutdown the set
/// is aborted so no orphaned embedding calls outlive the process (Bug 16).
pub fn spawn_workspace_watcher(
    workspace_dir: String,
    memory: std::sync::Arc<MemoryStore>,
    handle: tokio::runtime::Handle,
    cancel: tokio_util::sync::CancellationToken,
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
        // Bug 16: track in-flight index tasks so we can abort them on shutdown.
        let mut index_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            // F106: reap finished index tasks each iteration. tokio's JoinSet does
            // not free a completed task's node until it is joined, so without this
            // drain every debounce flush leaks an entry for the process lifetime
            // (and the shutdown `index_tasks.len()` over-reports in-flight work).
            while index_tasks.try_join_next().is_some() {}

            // Bug 12: cap recv_timeout to 250 ms so the cancel check is polled
            // frequently even when no FS events arrive.
            let timeout = debounce_deadline
                .map_or(std::time::Duration::from_millis(250), |d| {
                    d.saturating_duration_since(std::time::Instant::now())
                        .min(std::time::Duration::from_millis(250))
                });

            if cancel.is_cancelled() {
                let pending = index_tasks.len();
                if pending > 0 {
                    tracing::info!(pending, "aborting in-flight memory index tasks on shutdown");
                    // JoinSet::abort_all is sync — safe to call from an OS thread.
                    index_tasks.abort_all();
                }
                tracing::debug!("workspace watcher: shutdown signal received, exiting");
                break;
            }

            match rx.recv_timeout(timeout) {
                Ok(Ok(Event { kind: EventKind::Create(_) | EventKind::Modify(_), paths, .. })) => {
                    let exclude_dirs = crate::agent::workspace::MEMORY_INDEX_EXCLUDE_DIRS;
                    for p in paths {
                        // Skip files in system directories. Check ANY path
                        // component — belt-and-suspenders over strip_prefix
                        // against the canonical watch_dir, so a nested file
                        // like `workspace/agents/Opex/SOUL.md` is caught even
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
                        // Filename-level exclude (governance docs + composite
                        // suffixes like `.excalidraw.md`) + extension check.
                        let indexable = p
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(crate::agent::workspace::is_indexable_filename);
                        if indexable {
                            pending_files.insert(p);
                        }
                    }
                    if !pending_files.is_empty() {
                        debounce_deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                    }
                }
                Ok(_) => {} // other events
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Check whether the debounce deadline has actually expired —
                    // the 250 ms cap means we may arrive here before the 5 s
                    // debounce period ends.
                    let deadline_expired = debounce_deadline
                        .is_some_and(|d| std::time::Instant::now() >= d);

                    if deadline_expired && !pending_files.is_empty() {
                        let files: Vec<std::path::PathBuf> = pending_files.drain().collect();
                        let mem = memory.clone();
                        let workspace_dir_clone = workspace_dir.clone();
                        // Bug 16: spawn into the tracked JoinSet so handles are not dropped.
                        let _handle_ref = handle.enter();
                        index_tasks.spawn_on(async move {
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
                                // F065: reindex_source embeds FIRST and only
                                // deletes the old chunks after embedding
                                // succeeds — so an embedding blip leaves the
                                // file's existing chunks intact (still
                                // searchable) instead of wiping them. Warn (not
                                // debug) so the skipped update is observable.
                                match mem.reindex_source(&content, &source, false, "shared", "").await {
                                    Ok(_) => indexed += 1,
                                    Err(e) => {
                                        tracing::warn!(source = %source, error = %e, "embedding unavailable — kept existing chunks, skipping re-index (will retry on next edit)");
                                        break;
                                    }
                                }
                            }
                            if indexed > 0 {
                                tracing::info!(count = indexed, "workspace watcher: re-indexed changed files");
                            }
                        }, &handle);
                        debounce_deadline = None;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}
