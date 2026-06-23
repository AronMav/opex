use axum::{
    Router,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/workspace", get(api_workspace_browse))
        .route("/api/workspace/{*path}", get(api_workspace_browse).put(api_workspace_write).delete(api_workspace_delete))
}

/// Resolve and validate a path within the workspace/ directory.
/// Returns (`base_dir`, `target_path`) where target is guaranteed strictly inside workspace.
async fn resolve_workspace_path(rel_path: &str) -> Result<(std::path::PathBuf, std::path::PathBuf), (StatusCode, Json<Value>)> {
    let base = std::path::Path::new(crate::config::WORKSPACE_DIR);
    let _ = tokio::fs::create_dir_all(base).await;
    let target = base.join(rel_path);

    let base_canonical = tokio::fs::canonicalize(base).await
        .unwrap_or_else(|_| base.to_path_buf());

    let target_canonical = if target.exists() {
        tokio::fs::canonicalize(&target).await
            .unwrap_or_else(|_| target.clone())
    } else {
        // For new files, canonicalize parent
        if let Some(parent) = target.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
            let parent_canonical = tokio::fs::canonicalize(parent).await
                .unwrap_or_else(|_| parent.to_path_buf());
            let file_name = target.file_name().unwrap_or_default();
            parent_canonical.join(file_name)
        } else {
            target.clone()
        }
    };

    // Strictly require paths inside workspace — no install dir escape via symlinks
    if !target_canonical.starts_with(&base_canonical) {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "path traversal denied"}))));
    }

    Ok((base_canonical, target_canonical))
}

/// List directory contents as JSON entries.
async fn list_dir_entries(dir: &std::path::Path) -> Result<Vec<Value>, String> {
    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(dir).await.map_err(|e| e.to_string())?;

    while let Some(entry) = read_dir.next_entry().await.map_err(|e| e.to_string())? {
        let name = entry.file_name().to_string_lossy().to_string();
        // tokio::fs::metadata follows symlinks; fall back to DirEntry::metadata on error
        let path = entry.path();
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(_) => entry.metadata().await.map_err(|e| e.to_string())?,
        };
        let is_dir = metadata.is_dir();
        let size = metadata.len();
        let suffix = if is_dir { "/" } else { "" };
        let display = format!("{}{} ({})", name, suffix, format_workspace_size(size));
        entries.push(json!({ "name": name, "is_dir": is_dir, "display": display }));
    }
    entries.sort_by(|a, b| {
        let a_dir = a["is_dir"].as_bool().unwrap_or(false);
        let b_dir = b["is_dir"].as_bool().unwrap_or(false);
        b_dir.cmp(&a_dir).then_with(|| {
            a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
        })
    });
    Ok(entries)
}

pub(crate) fn format_workspace_size(bytes: u64) -> String {
    if bytes < 1024 { format!("{bytes} B") }
    else if bytes < 1024 * 1024 { format!("{:.1} KB", bytes as f64 / 1024.0) }
    else { format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)) }
}

/// Browse workspace: GET /api/workspace or GET /api/workspace/{path}
/// Directories return file list; files return content.
pub(crate) async fn api_workspace_browse(
    path: Option<axum::extract::Path<String>>,
) -> impl IntoResponse {
    let rel_path = path.as_ref().map_or(".", |p| p.as_str());

    let (_, target) = match resolve_workspace_path(if rel_path.is_empty() { "." } else { rel_path }).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    if target.is_dir() {
        match list_dir_entries(&target).await {
            Ok(entries) => Json(json!({ "files": entries, "is_dir": true })).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
        }
    } else if target.is_file() {
        match tokio::fs::read_to_string(&target).await {
            Ok(content) => Json(json!({ "content": content, "path": rel_path, "is_dir": false })).into_response(),
            Err(e) => {
                let status = if e.kind() == std::io::ErrorKind::NotFound { StatusCode::NOT_FOUND } else { StatusCode::INTERNAL_SERVER_ERROR };
                (status, Json(json!({"error": e.to_string()}))).into_response()
            }
        }
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkspaceWriteRequest {
    content: String,
}

pub(crate) async fn api_workspace_write(
    axum::extract::Path(rel_path): axum::extract::Path<String>,
    Json(req): Json<WorkspaceWriteRequest>,
) -> impl IntoResponse {
    let (_, target) = match resolve_workspace_path(&rel_path).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    // Ensure parent dirs exist
    if let Some(parent) = target.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    match tokio::fs::write(&target, &req.content).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_workspace_delete(
    axum::extract::Path(rel_path): axum::extract::Path<String>,
) -> impl IntoResponse {
    let (_, target) = match resolve_workspace_path(&rel_path).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    if target.is_dir() {
        // Only allow deleting empty directories (safety: no recursive delete).
        // ENOTEMPTY = 39 on Linux, 145 on Windows.
        match tokio::fs::remove_dir(&target).await {
            Ok(()) => Json(json!({"ok": true})).into_response(),
            Err(e) if matches!(e.raw_os_error(), Some(39 | 145)) => {
                (StatusCode::CONFLICT, Json(json!({"error": "Directory is not empty"}))).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
        }
    } else {
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Json(json!({"ok": true})).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
        }
    }
}
