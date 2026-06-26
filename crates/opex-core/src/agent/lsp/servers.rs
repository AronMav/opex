//! Language-server registry + project-root resolution (bounded to the agent dir).
use serde_json::json;
use std::path::{Path, PathBuf};

use crate::agent::workspace;

/// Describes a language server to launch for a given file.
#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed in Task 6/7
pub struct ServerDef {
    pub language: &'static str,
    pub command: Vec<String>,
    pub root_markers: Vec<&'static str>,
    pub init_options: serde_json::Value,
}

/// Return the [`ServerDef`] appropriate for the file extension in `rel`, or
/// `None` if no supported server covers that extension.
///
/// v1: Python only (pyright). TypeScript and Rust are v2.
#[allow(dead_code)] // consumed in Task 6/7
pub fn server_for_path(rel: &str) -> Option<ServerDef> {
    let ext = Path::new(rel).extension()?.to_str()?;
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    Some(match ext {
        "py" => ServerDef {
            language: "python",
            command: s(&["pyright-langserver", "--stdio"]),
            root_markers: vec!["pyproject.toml", "setup.py", "requirements.txt"],
            init_options: json!({}),
        },
        _ => return None, // TS (RCE) + Rust (toolchain) = v2
    })
}

/// Return the HOST-absolute project-root directory for `file_rel`, walking up
/// from the file's directory to the nearest directory that contains one of the
/// `markers`, never above `agents/{agent_name}/` (security boundary).
///
/// Falls back to the file's own directory when no marker is found.
#[allow(dead_code)] // consumed in Task 6/7
pub async fn resolve_project_root(
    workspace_dir: &str,
    agent_name: &str,
    file_rel: &str,
    markers: &[&str],
) -> anyhow::Result<PathBuf> {
    let abs = workspace::validate_workspace_path(workspace_dir, agent_name, file_rel).await?;
    let ws_abs =
        dunce::canonicalize(workspace_dir).unwrap_or_else(|_| Path::new(workspace_dir).to_path_buf());
    let floor = ws_abs.join("agents").join(agent_name);
    let file_dir = abs.parent().unwrap_or(&abs).to_path_buf();
    let mut dir = file_dir.clone();
    loop {
        if markers.iter().any(|m| dir.join(m).exists()) {
            return Ok(dir);
        }
        match dir.parent() {
            Some(p) if p.starts_with(&floor) || p == floor => dir = p.to_path_buf(),
            _ => return Ok(file_dir),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_server_by_ext() {
        assert_eq!(server_for_path("p/app.py").unwrap().language, "python");
        assert!(server_for_path("p/x.ts").is_none()); // TS = v2
        assert!(server_for_path("notes.md").is_none());
        assert!(server_for_path("p/main.rs").is_none()); // Rust = v2
    }

    #[tokio::test]
    async fn root_is_nearest_marker_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        let proj = ws.join("agents").join("A").join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("pyproject.toml"), "").unwrap();
        let root = resolve_project_root(
            ws.to_str().unwrap(),
            "A",
            "proj/src/main.py",
            &["pyproject.toml"],
        )
        .await
        .unwrap();
        assert!(root.ends_with("agents/A/proj"), "got {}", root.display());
    }

    #[tokio::test]
    async fn root_falls_back_to_file_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(ws.join("agents").join("A").join("proj")).unwrap();
        let root = resolve_project_root(
            ws.to_str().unwrap(),
            "A",
            "proj/main.py",
            &["pyproject.toml"],
        )
        .await
        .unwrap();
        assert!(root.ends_with("agents/A/proj"));
    }
}
