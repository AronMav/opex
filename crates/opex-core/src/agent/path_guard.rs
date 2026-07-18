//! Phase 64 SEC-02 — workspace path canonicalization guard.
//!
//! Leaf module: zero `crate::*` deps (only `std` and `dunce`). Safe to
//! re-export via `lib.rs` without cascading the agent subtree.
//!
//! The public entry point is [`resolve_workspace_path`], which accepts a
//! `workspace_dir` (assumed to exist) plus a user-supplied `Path` and
//! returns the canonical on-disk form, guaranteed to live under the
//! canonical workspace root.
//!
//! Why `dunce::canonicalize` rather than `std::fs::canonicalize`:
//!   - On Windows, `std::fs::canonicalize` emits UNC paths prefixed with
//!     `\\?\`, which break naive `starts_with(root)` comparisons when the
//!     root was obtained by user-friendly means.
//!   - `dunce::canonicalize` strips the `\\?\` prefix when the resulting
//!     path remains representable as a standard Windows path, otherwise
//!     preserves it — producing a consistent form.
//!   - On Unix, `dunce::canonicalize` is a transparent wrapper around
//!     `std::fs::canonicalize`, so behaviour is unchanged.

use std::path::{Component, Path, PathBuf};

/// Canonicalize a user-supplied path under `workspace_dir`.
///
/// * Relative paths are joined onto the canonical workspace root BEFORE
///   canonicalization, so `..` traversal surfaces during resolution rather
///   than as a string check.
/// * Rejects any `file_name` component equal to `..` or containing a path
///   separator — both are symptoms of a malformed probe that should never
///   reach the filesystem layer.
/// * For probe paths where the leaf file does not yet exist, the parent
///   directory is canonicalized and the leaf is reattached. This catches
///   symlink-based traversal even for files that have not been created.
/// * Fails closed with [`std::io::ErrorKind::PermissionDenied`] when the
///   canonical form escapes the workspace root.
//
// NOTE: production write/edit paths now inline an equivalent parent-canonicalize
// guard (see write_workspace_file) after e205c1f6 removed the re-join that caused
// a workspace/workspace/... double prefix. This function is retained as the
// reference implementation exercised by the path-canonicalization contract tests
// (tests/integration_path_canonicalize.rs + the unit tests below); hence
// allow(dead_code) for the bin target, which does not compile the test callers.
// FOLLOW-UP: retarget those tests at the inline guard and delete this, or
// re-expose it as the shared guard.
#[allow(dead_code)]
pub fn resolve_workspace_path(
    workspace_dir: &str,
    user_supplied: &Path,
) -> std::io::Result<PathBuf> {
    let root = dunce::canonicalize(Path::new(workspace_dir))?;

    // Reject a file_name component equal to ".." or one that embeds a path
    // separator. These are never legitimate leaf names.
    if let Some(name) = user_supplied.file_name() {
        let bytes = name.to_string_lossy();
        if bytes == ".." || bytes.contains('/') || bytes.contains('\\') || bytes.contains('\0') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid leaf component: {}", bytes.escape_default()),
            ));
        }
    }

    let joined = if user_supplied.is_absolute() {
        user_supplied.to_path_buf()
    } else {
        root.join(user_supplied)
    };

    let canonical = match dunce::canonicalize(&joined) {
        Ok(c) => c,
        Err(_) => {
            // Leaf may not exist yet — canonicalize parent, reattach leaf.
            let parent = joined.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "no parent component",
                )
            })?;
            let parent_canon = dunce::canonicalize(parent)?;

            // Bug 11: reject if the canonical parent itself escapes the root
            // (a symlink in the parent chain could make it resolve to /etc/...).
            if !parent_canon.starts_with(&root) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "parent path escapes workspace: {}",
                        parent_canon.display()
                    ),
                ));
            }

            // Bug 11: walk every ancestor of `joined` that is inside the workspace
            // root and reject if any is a symlink.  We only need to inspect
            // components between `root` and the leaf; components at or above
            // `root` were already resolved by `dunce::canonicalize(root)`.
            let mut cursor = root.clone();
            for component in joined.components() {
                // Prefix / RootDir / CurDir / ParentDir were already handled
                // by earlier guards (dotdot check) or are part of the root.
                if let Component::Normal(seg) = component {
                    cursor.push(seg);
                    // Only inspect components that actually exist on disk.
                    if cursor.exists() {
                        let meta = cursor.symlink_metadata().map_err(|e| {
                            std::io::Error::new(e.kind(), format!("symlink_metadata failed: {e}"))
                        })?;
                        if meta.file_type().is_symlink() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                format!(
                                    "symlink in path ancestry is not permitted: {}",
                                    cursor.display()
                                ),
                            ));
                        }
                    }
                }
            }

            let file = joined.file_name().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "no file component",
                )
            })?;
            parent_canon.join(file)
        }
    };

    if !canonical.starts_with(&root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("path escapes workspace: {}", canonical.display()),
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn absolute_inside_ws_allowed() {
        let ws = TempDir::new().unwrap();
        let p = ws.path().join("a.md");
        fs::write(&p, b"x").unwrap();
        let r = resolve_workspace_path(ws.path().to_str().unwrap(), &p).unwrap();
        assert!(r.starts_with(dunce::canonicalize(ws.path()).unwrap()));
    }

    #[test]
    fn relative_dotdot_blocked() {
        let ws = TempDir::new().unwrap();
        assert!(resolve_workspace_path(ws.path().to_str().unwrap(), Path::new("../x")).is_err());
    }

    #[test]
    fn leaf_with_separator_rejected() {
        let ws = TempDir::new().unwrap();
        // "../x" as a single component is not possible via Path, but a leaf
        // file_name that LITERALLY contains a separator (e.g. constructed
        // from OsString) must be rejected. We test the API surface: any
        // Path whose file_name equals ".." is refused.
        let bad = Path::new("..");
        let result = resolve_workspace_path(ws.path().to_str().unwrap(), bad);
        assert!(result.is_err(), "dotdot-only path must be rejected");
    }

    // Bug 11: a symlink directory inside the workspace pointing outside must
    // be rejected even when the leaf doesn't exist yet (the "parent canon +
    // reattach" code path).
    #[cfg(unix)]
    #[test]
    fn symlink_parent_dir_blocked() {
        use std::os::unix::fs::symlink;

        let outside = TempDir::new().unwrap();
        let ws = TempDir::new().unwrap();

        // Create a subdirectory inside the workspace that is actually a
        // symlink pointing to a directory outside the workspace.
        let link_dir = ws.path().join("subdir");
        symlink(outside.path(), &link_dir).unwrap();

        // Attempt to write a (non-existent) file through the symlinked dir.
        let probe = Path::new("subdir/secret.txt");
        let result = resolve_workspace_path(ws.path().to_str().unwrap(), probe);
        assert!(
            result.is_err(),
            "path through symlink dir must be rejected; got: {:?}",
            result
        );
    }
}
