//! Rewrite host paths in MCP tool arguments into container mount points.
//!
//! OPEX agents live on the host filesystem and naturally construct absolute
//! paths like `/home/aronmav/opex/workspace/agents/Arty/...`. The MCP
//! filesystem container only mounts the workspace at `/workspace`; passing host
//! paths results in "Access denied - path outside allowed directories".
//! Similarly, the git container mounts the deploy source tree at `/src` and
//! defaults to `/src` as its repository.
//!
//! This module rewrites tool arguments before they cross the MCP boundary.
//! Unknown absolute host paths are rejected with a clear error so the agent
//! can fall back to the correct system tool (e.g. `workspace_read`).

use std::path::Path;

/// Path-like argument keys that filesystem/git MCP tools commonly accept.
const PATH_KEYS: &[&str] = &[
    "path",
    "paths",
    "file_path",
    "directory_path",
    "dir_path",
    "repo_path",
    "old_path",
    "new_path",
    "source_path",
    "target_path",
    "from",
    "to",
];

/// Container mount point for the workspace directory.
const WORKSPACE_MOUNT: &str = "/workspace";

/// Container mount point for the source directory (mcp-git only).
const SOURCE_MOUNT: &str = "/src";

/// Rewrite tool arguments for filesystem/git MCPs.
///
/// - Strips any host path prefix that matches `workspace_dir` and replaces it
///   with `/workspace`.
/// - Strips any host path prefix that matches `source_mount_dir` (if provided)
///   and replaces it with `/src`.
/// - Relative paths are interpreted as relative to the workspace root.
/// - Unknown absolute host paths are rejected with a descriptive error.
pub fn rewrite_tool_arguments(
    mcp_name: &str,
    tool_name: &str,
    arguments: &serde_json::Value,
    workspace_dir: &Path,
    source_mount_dir: Option<&Path>,
) -> anyhow::Result<serde_json::Value> {
    let mut out = arguments.clone();
    let Some(obj) = out.as_object_mut() else {
        return Ok(out);
    };

    for key in PATH_KEYS {
        if let Some(value) = obj.get_mut(*key) {
            *value = rewrite_value(value, mcp_name, tool_name, workspace_dir, source_mount_dir)?;
        }
    }

    Ok(out)
}

fn rewrite_value(
    value: &serde_json::Value,
    mcp_name: &str,
    tool_name: &str,
    workspace_dir: &Path,
    source_mount_dir: Option<&Path>,
) -> anyhow::Result<serde_json::Value> {
    match value {
        serde_json::Value::String(s) => Ok(serde_json::Value::String(rewrite_path(
            s,
            mcp_name,
            tool_name,
            workspace_dir,
            source_mount_dir,
        )?)),
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(rewrite_value(item, mcp_name, tool_name, workspace_dir, source_mount_dir)?);
            }
            Ok(out.into())
        }
        _ => Ok(value.clone()),
    }
}

fn rewrite_path(
    path: &str,
    mcp_name: &str,
    tool_name: &str,
    workspace_dir: &Path,
    source_mount_dir: Option<&Path>,
) -> anyhow::Result<String> {
    // Already a container path — leave it alone.
    if path.starts_with(WORKSPACE_MOUNT) || path.starts_with(SOURCE_MOUNT) {
        return Ok(path.to_string());
    }

    // Relative paths are interpreted relative to the workspace root.
    if !is_absolute(path) {
        let joined = join_unix(WORKSPACE_MOUNT, path);
        return Ok(joined);
    }

    // Host workspace prefix -> /workspace.
    if let Some(relative) = strip_prefix(path, workspace_dir) {
        return Ok(join_unix(WORKSPACE_MOUNT, &relative));
    }

    // Host source prefix -> /src (only if a source mount is configured).
    if let Some(source_dir) = source_mount_dir
        && let Some(relative) = strip_prefix(path, source_dir)
    {
        return Ok(join_unix(SOURCE_MOUNT, &relative));
    }

    // Unknown absolute host path. Reject instead of letting the MCP container
    // fail with a confusing "not in /workspace" message.
    anyhow::bail!(
        "MCP tool '{tool_name}' on '{mcp_name}' received an absolute host path outside the allowed mounts: {path}. \
         Use '/workspace/...' for workspace files or '/src/...' for the source tree; use workspace_read/workspace_write for host paths that are not mounted."
    )
}

/// Strip a directory prefix from a path string, returning the remainder as a
/// Unix-style relative path (forward slashes, no leading slash).
fn strip_prefix(path: &str, prefix: &Path) -> Option<String> {
    let prefix_str = prefix.to_string_lossy();
    let prefix_norm = normalize_path(&prefix_str);
    let path_norm = normalize_path(path);

    if path_norm == prefix_norm {
        return Some(String::new());
    }

    let prefix_with_sep = format!("{prefix_norm}/");
    if let Some(rest) = path_norm.strip_prefix(&prefix_with_sep) {
        return Some(rest.to_string());
    }

    None
}

/// Join a Unix mount point with a relative path string. Produces forward-slash
/// paths regardless of host OS.
fn join_unix(mount: &str, relative: &str) -> String {
    let rel = normalize_path(relative);
    if rel.is_empty() {
        mount.to_string()
    } else {
        format!("{mount}/{rel}")
    }
}

/// True for absolute Unix paths or Windows drive-letter paths.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || path.chars().nth(1).is_some_and(|c| c == ':')
}

/// Normalize a path for string-prefix comparison: collapse multiple separators,
/// resolve `.` and `..` segments, drop trailing slashes, and use forward slashes.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." && !parts.is_empty() {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ws() -> PathBuf {
        PathBuf::from("/home/aronmav/opex/workspace")
    }

    fn src() -> PathBuf {
        PathBuf::from("/home/aronmav/opex-src")
    }

    #[test]
    fn workspace_absolute_becomes_container_path() {
        let out = rewrite_path(
            "/home/aronmav/opex/workspace/agents/Arty/journal.md",
            "mcp-filesystem",
            "read_file",
            &ws(),
            Some(&src()),
        )
        .unwrap();
        assert_eq!(out, "/workspace/agents/Arty/journal.md");
    }

    #[test]
    fn relative_path_is_workspace_relative() {
        let out = rewrite_path("agents/Arty/journal.md", "mcp-filesystem", "read_file", &ws(), Some(&src())).unwrap();
        assert_eq!(out, "/workspace/agents/Arty/journal.md");
    }

    #[test]
    fn source_path_becomes_src_mount() {
        let out = rewrite_path(
            "/home/aronmav/opex-src/ui/src/app/page.tsx",
            "mcp-git",
            "git_status",
            &ws(),
            Some(&src()),
        )
        .unwrap();
        assert_eq!(out, "/src/ui/src/app/page.tsx");
    }

    #[test]
    fn container_path_passthrough() {
        assert_eq!(
            rewrite_path("/workspace/foo.md", "mcp-filesystem", "read_file", &ws(), Some(&src())).unwrap(),
            "/workspace/foo.md"
        );
        assert_eq!(
            rewrite_path("/src/ui", "mcp-git", "git_status", &ws(), Some(&src())).unwrap(),
            "/src/ui"
        );
    }

    #[test]
    fn outside_mount_rejected() {
        let err = rewrite_path(
            "/etc/passwd",
            "mcp-filesystem",
            "read_file",
            &ws(),
            Some(&src()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("outside the allowed mounts"));
    }

    #[test]
    fn array_of_paths_rewritten() {
        let args = serde_json::json!({
            "paths": [
                "/home/aronmav/opex/workspace/AGENTS.md",
                "agents/Arty/journal.md"
            ]
        });
        let out = rewrite_tool_arguments(
            "mcp-filesystem",
            "read_multiple_files",
            &args,
            &ws(),
            Some(&src()),
        )
        .unwrap();
        let expected = serde_json::json!({
            "paths": [
                "/workspace/AGENTS.md",
                "/workspace/agents/Arty/journal.md"
            ]
        });
        assert_eq!(out, expected);
    }

    #[test]
    fn windows_style_path_normalized() {
        let out = rewrite_path(
            "\\home\\aronmav\\opex\\workspace\\agents\\Arty\\journal.md",
            "mcp-filesystem",
            "read_file",
            &ws(),
            Some(&src()),
        )
        .unwrap();
        assert_eq!(out, "/workspace/agents/Arty/journal.md");
    }
}
