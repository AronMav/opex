//! Phase 64 SEC-02 — path canonicalization cross-platform matrix.
//!
//! CRITICAL: file MUST compile on all three OSes. Runtime-only platform
//! checks use `if cfg!(windows) { ... }` NOT `#[cfg(windows)]` where
//! possible, so CI runs one binary per-OS and skips per-runtime.
//!
//! Contract under test: `hydeclaw_core::agent::path_guard::resolve_workspace_path`
//!   - Relative `..` traversal is rejected on every OS.
//!   - Symlinks whose canonical target escapes the workspace root are rejected.
//!     (Unix: unconditional. Windows: requires Developer Mode; if symlink
//!     creation fails the test skips — never fails.)
//!   - Mixed-case input on Windows canonicalizes to the on-disk form.
//!   - UNC-prefixed Windows paths are either normalized into the workspace
//!     or rejected — they MUST NOT silently bypass `starts_with(root)`.

mod support;

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

use hydeclaw_core::agent::path_guard::resolve_workspace_path;

#[test]
fn dotdot_traversal_blocked() {
    let ws = TempDir::new().unwrap();
    fs::create_dir_all(ws.path().join("inner")).unwrap();
    let escape = PathBuf::from("../../etc/passwd");
    let result = resolve_workspace_path(ws.path().to_str().unwrap(), &escape);
    assert!(
        result.is_err(),
        "dotdot must be blocked, got {result:?}"
    );
}

#[test]
#[cfg(unix)]
fn symlink_escape_blocked_unix() {
    let ws = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret.txt"), b"x").unwrap();
    std::os::unix::fs::symlink(outside.path(), ws.path().join("escape")).unwrap();
    let probe = PathBuf::from("escape").join("secret.txt");
    let result = resolve_workspace_path(ws.path().to_str().unwrap(), &probe);
    assert!(
        result.is_err(),
        "symlink escape must be blocked, got {result:?}"
    );
}

#[test]
#[cfg(windows)]
fn symlink_escape_blocked_windows() {
    use std::os::windows::fs::symlink_dir;
    let ws = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret.txt"), b"x").unwrap();
    if symlink_dir(outside.path(), ws.path().join("escape")).is_err() {
        eprintln!("skip: Windows symlink creation needs Developer Mode or admin");
        return;
    }
    let probe = PathBuf::from("escape").join("secret.txt");
    let result = resolve_workspace_path(ws.path().to_str().unwrap(), &probe);
    assert!(
        result.is_err(),
        "windows symlink escape must be blocked, got {result:?}"
    );
}

#[test]
fn mixed_case_path_stable() {
    let ws = TempDir::new().unwrap();
    let p = ws.path().join("hello.md");
    fs::write(&p, b"hi").unwrap();
    let a = resolve_workspace_path(ws.path().to_str().unwrap(), &PathBuf::from("hello.md"))
        .expect("lower-case path resolves");
    if cfg!(windows) {
        let b = resolve_workspace_path(ws.path().to_str().unwrap(), &PathBuf::from("Hello.md"))
            .expect("mixed-case resolves on windows");
        assert_eq!(
            a, b,
            "windows case-insensitive — canonical forms must match"
        );
    }
}

/// `resolve_workspace_path` must reject paths with a null byte. The OS path
/// canonicalization layer will reject them first, but the guard must surface the
/// error rather than truncate silently or return an unexpected path.
#[test]
fn null_byte_in_path_blocked() {
    let ws = TempDir::new().unwrap();
    // Null byte embedded in a filename component — never a valid path on any
    // supported OS (POSIX, Windows, macOS all treat it as a terminator).
    let bad = PathBuf::from("foo\0bar.md");
    let result = resolve_workspace_path(ws.path().to_str().unwrap(), &bad);
    assert!(
        result.is_err(),
        "null byte in path must be rejected, got {result:?}"
    );
}

/// URL-percent-encoded traversal (`..%2F`) should NOT produce a dot-dot
/// component after the OS resolves the path. The guard sees the raw
/// (not-yet-decoded) string as a `Path`; on all target OSes the literal
/// string `"..%2F"` is treated as a single filename component (not a `..`
/// followed by a slash), so `resolve_workspace_path` should accept it as a
/// normal (non-existent) filename inside the workspace without escaping.
///
/// This differs from the shell expansion case and documents that the guard
/// does NOT do URL-percent decoding itself — callers must decode first.
#[test]
fn percent_encoded_traversal_stays_inside_workspace() {
    let ws = TempDir::new().unwrap();
    // "..%2F" as a raw filesystem component is just an unusual filename, not
    // an escape. Either it stays inside (Ok) or is rejected for another
    // reason — it must never escape the workspace root.
    let encoded = PathBuf::from("..%2Fetc%2Fpasswd");
    let result = resolve_workspace_path(ws.path().to_str().unwrap(), &encoded);
    if let Ok(ref p) = result {
        let root = dunce::canonicalize(ws.path()).unwrap();
        assert!(
            p.starts_with(&root),
            "percent-encoded path must not escape workspace: {p:?}"
        );
    }
    // Err is also acceptable (file does not exist, or invalid component).
}

#[test]
fn unc_or_standard_windows_path() {
    let ws = TempDir::new().unwrap();
    let p = ws.path().join("foo.md");
    fs::write(&p, b"x").unwrap();
    if cfg!(windows) {
        let s = format!("\\\\?\\{}", p.display());
        let result = resolve_workspace_path(ws.path().to_str().unwrap(), &PathBuf::from(&s));
        // UNC prefix must either canonicalize into the workspace or be rejected —
        // never silently bypass `starts_with`. Both outcomes satisfy the contract.
        if let Ok(p2) = result {
            let root = dunce::canonicalize(ws.path()).unwrap();
            assert!(p2.starts_with(&root), "UNC escaped ws: {p2:?}");
        }
    }
}
