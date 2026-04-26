//! Workspace artifact tracking for the code-execution tool.
//!
//! `snapshot()` walks `workspace_dir` once, hashing each whitelisted
//! file with SHA-256. `diff()` compares two snapshots and returns
//! Created + Modified changes (deletions are intentionally dropped —
//! no UI use case in v1).
//!
//! Used by `agent/pipeline/sandbox.rs` to detect files produced by
//! scripts run inside the sandbox. `workspace_write`/`workspace_edit`
//! do NOT use this — they know their target path synchronously.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct WorkspaceSnapshot {
    /// Map from path relative to workspace_dir to SHA-256 hash.
    pub(crate) entries: HashMap<PathBuf, [u8; 32]>,
    /// True if scan was truncated (limits exceeded).
    pub(crate) truncated: bool,
}

#[derive(Debug, PartialEq)]
pub struct ArtifactChange {
    pub rel_path: PathBuf,
    pub kind: ChangeKind,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ChangeKind { Created, Modified }

pub(crate) const MAX_FILES: usize = 200;
pub(crate) const MAX_BYTES: u64 = 100 * 1024 * 1024;

pub(crate) const BLACKLIST_DIRS: &[&str] = &[
    "skills", "tools", "mcp", "prompts",
    ".git", "node_modules", "__pycache__", "target", ".venv", "venv",
];

pub(crate) const WHITELIST_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg",
    "pdf", "csv", "tsv", "json", "md", "txt",
    "html", "xml", "log", "py", "rs", "ts", "js",
    "yaml", "yml", "toml", "sql",
];

/// Snapshot all "user-artifact" files under `workspace_dir`.
pub fn snapshot(workspace_dir: &Path) -> WorkspaceSnapshot {
    let mut snap = WorkspaceSnapshot {
        entries: HashMap::new(),
        truncated: false,
    };
    let mut total_bytes: u64 = 0;
    walk(workspace_dir, workspace_dir, &mut snap, &mut total_bytes);
    snap
}

/// Compare two snapshots; return Created and Modified changes only.
pub fn diff(before: &WorkspaceSnapshot, after: &WorkspaceSnapshot) -> Vec<ArtifactChange> {
    let mut out = Vec::new();
    for (path, hash) in &after.entries {
        match before.entries.get(path) {
            None => out.push(ArtifactChange { rel_path: path.clone(), kind: ChangeKind::Created }),
            Some(prev) if prev != hash => {
                out.push(ArtifactChange { rel_path: path.clone(), kind: ChangeKind::Modified })
            }
            Some(_) => {}
        }
    }
    out
}

fn walk(root: &Path, current: &Path, snap: &mut WorkspaceSnapshot, total_bytes: &mut u64) {
    if snap.entries.len() >= MAX_FILES || *total_bytes >= MAX_BYTES {
        snap.truncated = true;
        return;
    }
    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(dir = %current.display(), error = %e, "artifact_hook: read_dir failed");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') { continue; }
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "artifact_hook: stat failed");
                continue;
            }
        };
        if meta.file_type().is_symlink() { continue; }
        if meta.is_dir() {
            if BLACKLIST_DIRS.contains(&name) { continue; }
            walk(root, &path, snap, total_bytes);
            if snap.truncated { return; }
            continue;
        }
        let ext_ok = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .map(|e| WHITELIST_EXTENSIONS.contains(&e.as_str()))
            .unwrap_or(false);
        if !ext_ok { continue; }
        let size = meta.len();
        if *total_bytes + size > MAX_BYTES {
            snap.truncated = true;
            tracing::warn!(path = %path.display(), size, total_bytes, "artifact_hook: snapshot truncated at MAX_BYTES");
            return;
        }
        let hash = match hash_file(&path) {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "artifact_hook: hash failed");
                continue;
            }
        };
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        snap.entries.insert(rel, hash);
        *total_bytes += size;
        if snap.entries.len() >= MAX_FILES {
            snap.truncated = true;
            tracing::warn!(count = snap.entries.len(), "artifact_hook: snapshot truncated at MAX_FILES");
            return;
        }
    }
}

fn hash_file(path: &Path) -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, rel: &str, content: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dir");
        }
        let mut f = std::fs::File::create(&path).expect("create file");
        f.write_all(content).expect("write");
    }

    #[test]
    fn snapshot_picks_up_user_files() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "out.csv", b"a,b\n1,2\n");
        write_file(dir.path(), "chart.png", b"\x89PNG\r\n");
        let snap = snapshot(dir.path());
        assert_eq!(snap.entries.len(), 2);
        assert!(snap.entries.contains_key(&PathBuf::from("out.csv")));
        assert!(snap.entries.contains_key(&PathBuf::from("chart.png")));
        assert!(!snap.truncated);
    }

    #[test]
    fn snapshot_skips_blacklisted_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "user.csv", b"x");
        write_file(dir.path(), "skills/system.md", b"x");
        write_file(dir.path(), "tools/t.yaml", b"x");
        write_file(dir.path(), ".git/config", b"x");
        let snap = snapshot(dir.path());
        assert_eq!(snap.entries.len(), 1);
        assert!(snap.entries.contains_key(&PathBuf::from("user.csv")));
    }

    #[test]
    fn snapshot_skips_non_whitelist_extensions() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "keep.csv", b"x");
        write_file(dir.path(), "drop.bin", b"x");
        write_file(dir.path(), "drop.exe", b"x");
        let snap = snapshot(dir.path());
        assert_eq!(snap.entries.len(), 1);
    }

    #[test]
    fn snapshot_skips_hidden_files() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.csv", b"x");
        write_file(dir.path(), ".cache", b"x");
        write_file(dir.path(), ".gitkeep", b"");
        let snap = snapshot(dir.path());
        assert_eq!(snap.entries.len(), 1);
    }

    #[test]
    fn snapshot_truncates_at_max_files() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_FILES + 5) {
            write_file(dir.path(), &format!("f{i}.txt"), b"x");
        }
        let snap = snapshot(dir.path());
        assert!(snap.entries.len() <= MAX_FILES);
        assert!(snap.truncated);
    }

    #[test]
    fn snapshot_truncates_at_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![0u8; 60 * 1024 * 1024];
        write_file(dir.path(), "a.txt", &big);
        write_file(dir.path(), "b.txt", &big);
        let snap = snapshot(dir.path());
        assert_eq!(snap.entries.len(), 1, "second file should be skipped");
        assert!(snap.truncated);
    }

    #[test]
    fn diff_detects_created_files() {
        let dir = tempfile::tempdir().unwrap();
        let before = snapshot(dir.path());
        write_file(dir.path(), "new.csv", b"data");
        let after = snapshot(dir.path());
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Created);
        assert_eq!(changes[0].rel_path, PathBuf::from("new.csv"));
    }

    #[test]
    fn diff_detects_modified_files_via_sha256() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "x.csv", b"v1");
        let before = snapshot(dir.path());
        write_file(dir.path(), "x.csv", b"v2");
        let after = snapshot(dir.path());
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Modified);
    }

    #[test]
    fn diff_no_change_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "x.csv", b"v1");
        let snap = snapshot(dir.path());
        let changes = diff(&snap, &snap);
        assert!(changes.is_empty());
    }

    #[test]
    fn diff_ignores_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "x.csv", b"v1");
        let before = snapshot(dir.path());
        std::fs::remove_file(dir.path().join("x.csv")).unwrap();
        let after = snapshot(dir.path());
        let changes = diff(&before, &after);
        assert!(changes.is_empty(), "deletions intentionally not surfaced");
    }

    #[test]
    fn diff_handles_delete_then_recreate_within_one_call() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "x.csv", b"v1");
        let before = snapshot(dir.path());
        std::fs::remove_file(dir.path().join("x.csv")).unwrap();
        write_file(dir.path(), "x.csv", b"v2");
        let after = snapshot(dir.path());
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Modified);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        write_file(target_dir.path(), "secret.csv", b"hidden");
        std::os::unix::fs::symlink(target_dir.path(), dir.path().join("link")).unwrap();
        write_file(dir.path(), "user.csv", b"x");
        let snap = snapshot(dir.path());
        assert_eq!(snap.entries.len(), 1);
        assert!(snap.entries.contains_key(&PathBuf::from("user.csv")));
    }
}
