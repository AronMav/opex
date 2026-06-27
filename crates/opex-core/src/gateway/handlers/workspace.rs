use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::{ConfigServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/workspace", get(api_workspace_browse))
        .route("/api/workspace/{*path}", get(api_workspace_browse).put(api_workspace_write).delete(api_workspace_delete))
}

/// Extensions treated as binary/media — browse returns a signed URL, never UTF-8.
const BINARY_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg", "pdf",
    "mp3", "wav", "ogg", "opus", "m4a", "mp4", "webm", "mov",
    "zip", "gz", "tar", "bin", "wasm",
];

pub(crate) fn is_binary_filename(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .map(|e| BINARY_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// Resolve and validate a path within `base`.
/// Returns `(base_canonical, target_canonical)` where target is guaranteed strictly inside base.
async fn resolve_within(
    base: &std::path::Path,
    rel_path: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), (StatusCode, Json<Value>)> {
    let _ = tokio::fs::create_dir_all(base).await;
    let target = base.join(rel_path);

    let base_canonical = tokio::fs::canonicalize(base).await
        .unwrap_or_else(|_| base.to_path_buf());

    let target_canonical = if target.exists() {
        tokio::fs::canonicalize(&target).await
            .unwrap_or_else(|_| target.clone())
    } else if let Some(parent) = target.parent() {
        // For new files, canonicalize parent
        let _ = tokio::fs::create_dir_all(parent).await;
        let parent_canonical = tokio::fs::canonicalize(parent).await
            .unwrap_or_else(|_| parent.to_path_buf());
        let file_name = target.file_name().unwrap_or_default();
        parent_canonical.join(file_name)
    } else {
        target.clone()
    };

    // Strictly require paths inside base — no escape via symlinks or traversal
    if !target_canonical.starts_with(&base_canonical) {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "path traversal denied"}))));
    }

    Ok((base_canonical, target_canonical))
}

/// Resolve and validate a path within the workspace/ directory.
/// Returns (`base_dir`, `target_path`) where target is guaranteed strictly inside workspace.
async fn resolve_workspace_path(
    rel_path: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), (StatusCode, Json<Value>)> {
    resolve_within(std::path::Path::new(crate::config::WORKSPACE_DIR), rel_path).await
}

/// Build a JSON response for a single workspace file.
///
/// Binary files (by extension or invalid UTF-8) return a signed URL;
/// text files return their content inline.
async fn build_file_response(
    base: &std::path::Path,
    rel: &str,
    key: &[u8; 32],
    ttl: u64,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let (base_canon, target) = resolve_within(base, rel).await?;

    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let bytes = tokio::fs::read(&target).await.map_err(|e| {
        let status = if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(json!({"error": e.to_string()})))
    })?;

    // Binary if extension says so OR content is not valid UTF-8.
    let is_binary = is_binary_filename(name) || std::str::from_utf8(&bytes).is_err();

    if is_binary {
        // Re-derive workspace-relative path so the signed URL matches what
        // serve_workspace_file canonicalizes (C-2 bug class).
        let rel_for_url = target
            .strip_prefix(&base_canon)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| rel.to_string());
        let url = crate::uploads::mint_workspace_file_url(&rel_for_url, key, ttl);
        let mime = crate::uploads::guess_mime_from_extension(name);
        Ok(json!({
            "is_binary": true,
            "mime": mime,
            "size": bytes.len(),
            "url": url,
            "path": rel,
            "is_dir": false,
        }))
    } else {
        let content = String::from_utf8(bytes).unwrap_or_default();
        Ok(json!({ "content": content, "path": rel, "is_dir": false }))
    }
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
/// Directories return file list; text files return content inline;
/// binary files return a signed URL.
pub(crate) async fn api_workspace_browse(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
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
        let key = infra.secrets.get_upload_hmac_key();
        let ttl = cfg.config.uploads.signed_url_ttl_secs;
        match build_file_response(std::path::Path::new(crate::config::WORKSPACE_DIR), rel_path, &key, ttl).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => e.into_response(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn build_response_text_returns_content() {
        let base = tempfile::tempdir().unwrap();
        tokio::fs::write(base.path().join("n.md"), b"# Hi").await.unwrap();
        let v = build_file_response(base.path(), "n.md", &[7u8; 32], 3600).await.unwrap();
        assert_eq!(v["content"], "# Hi");
        assert_eq!(v["is_dir"], false);
        assert!(v.get("is_binary").is_none());
    }

    #[tokio::test]
    async fn build_response_binary_returns_signed_url() {
        let base = tempfile::tempdir().unwrap();
        // 1-byte PNG-ish binary (invalid UTF-8 byte 0xFF ensures non-text).
        tokio::fs::write(base.path().join("img.png"), [0xFFu8, 0x00, 0x01]).await.unwrap();
        let v = build_file_response(base.path(), "img.png", &[7u8; 32], 3600).await.unwrap();
        assert_eq!(v["is_binary"], true);
        assert_eq!(v["mime"], "image/png");
        assert_eq!(v["size"], 3);
        let url = v["url"].as_str().unwrap();
        assert!(url.starts_with("/workspace-files/img.png?sig="), "got {url}");
    }

    #[tokio::test]
    async fn build_response_extensionless_binary_returns_signed_url() {
        let base = tempfile::tempdir().unwrap();
        tokio::fs::write(base.path().join("rawdata"), [0xFFu8]).await.unwrap();
        let v = build_file_response(base.path(), "rawdata", &[7u8; 32], 3600).await.unwrap();
        assert_eq!(v["is_binary"], true);
    }

    #[test]
    fn binary_classification_by_extension() {
        for n in ["a.png", "a.JPG", "photo.jpeg", "x.webp", "y.gif", "doc.pdf", "icon.svg"] {
            assert!(is_binary_filename(n), "{n} must be binary");
        }
        for n in ["note.md", "data.json", "cfg.toml", "log.txt", "s.yaml", "x.csv"] {
            assert!(!is_binary_filename(n), "{n} must be text");
        }
    }

    #[test]
    fn unknown_extension_defaults_to_text() {
        // No extension / unknown → treated as text (browse falls back to UTF-8 probe).
        assert!(!is_binary_filename("Makefile"));
        assert!(!is_binary_filename("weird.xyz"));
    }

    #[tokio::test]
    async fn resolve_within_rejects_traversal() {
        let base = tempfile::tempdir().unwrap();
        let res = resolve_within(base.path(), "../escape.txt").await;
        assert!(res.is_err(), "traversal must be denied");
    }

    #[tokio::test]
    async fn resolve_within_accepts_inside() {
        let base = tempfile::tempdir().unwrap();
        tokio::fs::write(base.path().join("ok.md"), b"hi").await.unwrap();
        let (b, t) = resolve_within(base.path(), "ok.md").await.unwrap();
        assert!(t.starts_with(&b));
    }
}
