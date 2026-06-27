use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use axum::extract::{DefaultBodyLimit, Multipart};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::{ConfigServices, InfraServices};

pub(crate) const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

pub(crate) fn routes() -> Router<AppState> {
    let upload = Router::new()
        .route("/api/workspace/upload", post(api_workspace_upload))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1024 * 1024));

    // PUT /api/workspace/{*path} accepts a JSON body; bound it to prevent large
    // payloads from reaching the handler.  GET and DELETE have no body, so the
    // layer is harmless for them.
    let workspace_catchall = Router::new()
        .route("/api/workspace/{*path}", get(api_workspace_browse).put(api_workspace_write).delete(api_workspace_delete))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1024 * 1024));

    Router::new()
        .route("/api/workspace", get(api_workspace_browse))
        .route("/api/workspace/sign", post(api_workspace_sign))
        .route("/api/workspace/mkdir", post(api_workspace_mkdir))
        .route("/api/workspace/rename", post(api_workspace_rename))
        .merge(workspace_catchall)
        .merge(upload)
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

/// Canonicalize `path` even when it (or trailing components) don't exist yet:
/// canonicalize the nearest EXISTING ancestor, then re-append the missing tail.
/// Read-only — creates nothing on disk. Resolves `..`/symlinks in the existing
/// prefix via the OS, so the traversal guard remains sound.
async fn canonicalize_existing_prefix(path: &std::path::Path) -> std::path::PathBuf {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if tokio::fs::metadata(&cur).await.is_ok() {
            let mut resolved = tokio::fs::canonicalize(&cur).await.unwrap_or_else(|_| cur.clone());
            for comp in tail.iter().rev() {
                resolved.push(comp);
            }
            return resolved;
        }
        match cur.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break,
                }
            }
            None => break, // ends in `..` or root with no file_name
        }
    }
    path.to_path_buf() // unreachable in practice — base always exists
}

/// Resolve and validate a path within `base`.
/// Returns `(base_canonical, target_canonical)` where target is guaranteed strictly inside base.
async fn resolve_within(
    base: &std::path::Path,
    rel_path: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), (StatusCode, Json<Value>)> {
    // Reject any `..` component up front. PathBuf::starts_with does NOT resolve
    // `..`, so a non-existent `..`-tail path could otherwise pass the
    // component-level guard (Linux traversal bypass). No legitimate workspace
    // path needs `..`.
    if rel_path.split(['/', '\\']).any(|seg| seg == "..") {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "path traversal denied"}))));
    }

    let _ = tokio::fs::create_dir_all(base).await; // workspace root — legitimately ensured
    let target = base.join(rel_path);

    let base_canonical = tokio::fs::canonicalize(base).await
        .unwrap_or_else(|_| base.to_path_buf());

    let target_canonical = if tokio::fs::metadata(&target).await.is_ok() {
        tokio::fs::canonicalize(&target).await.unwrap_or_else(|_| target.clone())
    } else {
        canonicalize_existing_prefix(&target).await
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

/// Files larger than this threshold that are NOT identified as binary by
/// extension are served as binary (signed URL) to avoid reading large blobs
/// into RAM during UTF-8 probing.
const MAX_TEXT_READ_BYTES: u64 = MAX_UPLOAD_BYTES as u64;

/// Build a JSON response for a single workspace file.
///
/// Binary files (by extension or invalid UTF-8) return a signed URL;
/// text files return their content inline.
///
/// Optimisation: files whose extension is in `BINARY_EXTS` are served via a
/// signed URL **without reading the file content** — only `metadata.len()` is
/// needed.  For unknown/text extensions, the file is read only if its size is
/// ≤ `MAX_TEXT_READ_BYTES`; larger files are treated as binary.
async fn build_file_response(
    base: &std::path::Path,
    rel: &str,
    key: &[u8; 32],
    ttl: u64,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let (base_canon, target) = resolve_within(base, rel).await?;

    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // Helper: build the binary JSON response given a file size.
    let binary_response = |size: u64| {
        let rel_for_url = target
            .strip_prefix(&base_canon)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| rel.to_string());
        let url = crate::uploads::mint_workspace_file_url(&rel_for_url, key, ttl);
        let mime = crate::uploads::guess_mime_from_extension(name);
        json!({
            "is_binary": true,
            "mime": mime,
            "size": size,
            "url": url,
            "path": rel,
            "is_dir": false,
        })
    };

    // Map a metadata / read IO error to the appropriate HTTP status.
    let map_io_err = |e: std::io::Error| {
        let status = if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(json!({"error": e.to_string()})))
    };

    if is_binary_filename(name) {
        // Known-binary extension: stat only — never read the file.
        let meta = tokio::fs::metadata(&target).await.map_err(map_io_err)?;
        return Ok(binary_response(meta.len()));
    }

    // Unknown / text extension: read, but cap at MAX_TEXT_READ_BYTES first.
    let meta = tokio::fs::metadata(&target).await.map_err(map_io_err)?;
    if meta.len() > MAX_TEXT_READ_BYTES {
        return Ok(binary_response(meta.len()));
    }

    let bytes = tokio::fs::read(&target).await.map_err(map_io_err)?;

    // Still might not be valid UTF-8 (e.g. `.bin` file, extensionless binary).
    if std::str::from_utf8(&bytes).is_err() {
        return Ok(binary_response(bytes.len() as u64));
    }

    let content = String::from_utf8(bytes).unwrap_or_default();
    Ok(json!({ "content": content, "path": rel, "is_dir": false }))
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

#[derive(Debug, Deserialize)]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    recursive: bool,
}

async fn do_delete(
    base: &std::path::Path,
    rel: &str,
    recursive: bool,
) -> Result<(), (StatusCode, Json<Value>)> {
    let (base_canon, target) = resolve_within(base, rel).await?;

    // Never delete the workspace root itself.
    if target == base_canon {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "cannot delete workspace root"}))));
    }

    if target.is_dir() {
        if recursive {
            tokio::fs::remove_dir_all(&target).await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
        } else {
            // ENOTEMPTY = 39 on Linux, 145 on Windows.
            match tokio::fs::remove_dir(&target).await {
                Ok(()) => Ok(()),
                Err(e) if matches!(e.raw_os_error(), Some(39 | 145)) => {
                    Err((StatusCode::CONFLICT, Json(json!({"error": "Directory is not empty"}))))
                }
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))),
            }
        }
    } else {
        tokio::fs::remove_file(&target).await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
    }
}

pub(crate) async fn api_workspace_delete(
    axum::extract::Path(rel_path): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<DeleteQuery>,
) -> impl IntoResponse {
    match do_delete(std::path::Path::new(crate::config::WORKSPACE_DIR), &rel_path, q.recursive).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SignRequest {
    paths: Vec<String>,
}

/// Build a map from requested path string → signed URL for every path that
/// resolves to an existing file inside `base`. External paths and missing
/// files are silently skipped (never 4xx the whole batch).
async fn build_sign_map(
    base: &std::path::Path,
    paths: &[String],
    key: &[u8; 32],
    ttl: u64,
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for p in paths {
        let Ok((base_canon, target)) = resolve_within(base, p).await else { continue };
        if !target.is_file() { continue }
        let rel_for_url = target
            .strip_prefix(&base_canon)
            .map(|x| x.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| p.clone());
        let url = crate::uploads::mint_workspace_file_url(&rel_for_url, key, ttl);
        out.insert(p.clone(), Value::String(url));
    }
    out
}

/// POST /api/workspace/sign — batch-sign inline asset URLs.
///
/// Body: `{ "paths": ["note/images/x.png", ...] }`
/// Response: `{ "url_by_path": { "note/images/x.png": "/workspace-files/...?sig=..." } }`
///
/// External paths and missing files are silently omitted from the map.
pub(crate) async fn api_workspace_sign(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
    Json(req): Json<SignRequest>,
) -> impl IntoResponse {
    let key = infra.secrets.get_upload_hmac_key();
    let ttl = cfg.config.uploads.signed_url_ttl_secs;
    let map = build_sign_map(
        std::path::Path::new(crate::config::WORKSPACE_DIR),
        &req.paths,
        &key,
        ttl,
    )
    .await;
    Json(json!({ "url_by_path": Value::Object(map) }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct MkdirRequest {
    path: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RenameRequest {
    from: String,
    to: String,
}

async fn do_mkdir(base: &std::path::Path, rel: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let (_, target) = resolve_within(base, rel).await?;
    tokio::fs::create_dir_all(&target).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
}

async fn do_rename(base: &std::path::Path, from: &str, to: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let (_, from_t) = resolve_within(base, from).await?;
    let (_, to_t) = resolve_within(base, to).await?;
    if to_t.exists() {
        return Err((StatusCode::CONFLICT, Json(json!({"error": "target already exists"}))));
    }
    if let Some(parent) = to_t.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    tokio::fs::rename(&from_t, &to_t).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
}

pub(crate) async fn api_workspace_mkdir(Json(req): Json<MkdirRequest>) -> impl IntoResponse {
    match do_mkdir(std::path::Path::new(crate::config::WORKSPACE_DIR), &req.path).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => e.into_response(),
    }
}

pub(crate) async fn api_workspace_rename(Json(req): Json<RenameRequest>) -> impl IntoResponse {
    match do_rename(
        std::path::Path::new(crate::config::WORKSPACE_DIR),
        &req.from,
        &req.to,
    )
    .await
    {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Validate, sanitize, and write a single uploaded file.
///
/// - Rejects bytes exceeding `MAX_UPLOAD_BYTES` with 413.
/// - Strips all directory components from `filename` (basename only) and
///   rejects an empty or dot-only result with 400.
/// - Writes to `<base>/<dir>/<basename>`, creating parent dirs as needed.
/// - Returns the saved workspace-relative path (e.g. `"sub/evil.png"`).
async fn save_upload(
    base: &std::path::Path,
    dir: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, (StatusCode, Json<Value>)> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"error": "file too large"}))));
    }
    let basename = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or((StatusCode::BAD_REQUEST, Json(json!({"error": "invalid filename"}))))?;

    let rel = if dir.is_empty() {
        basename.to_string()
    } else {
        format!("{}/{}", dir.trim_end_matches('/'), basename)
    };

    let (_, target) = resolve_within(base, &rel).await?;
    if let Some(parent) = target.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    tokio::fs::write(&target, bytes).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    Ok(rel)
}

/// POST /api/workspace/upload — multipart file upload.
///
/// Form fields (in order):
/// - `dir` (text) — workspace-relative target directory; empty string = workspace root.
/// - `file` (file, one or more) — files to upload.
///
/// Response: `{ "ok": bool, "saved": [rel_path, ...], "errors": [...] }`
pub(crate) async fn api_workspace_upload(mut multipart: Multipart) -> impl IntoResponse {
    let base = std::path::Path::new(crate::config::WORKSPACE_DIR);
    let mut dir = String::new();
    let mut saved: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "dir" {
            dir = field.text().await.unwrap_or_default();
        } else if name == "file" {
            let filename = field.file_name().unwrap_or("file").to_string();
            match field.bytes().await {
                Ok(bytes) => match save_upload(base, &dir, &filename, &bytes).await {
                    Ok(rel) => saved.push(rel),
                    Err((_, e)) => errors.push(format!("{}: {}", filename, e.0["error"])),
                },
                Err(e) => errors.push(format!("{filename}: {e}")),
            }
        }
    }
    Json(json!({ "ok": errors.is_empty(), "saved": saved, "errors": errors })).into_response()
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

    #[tokio::test]
    async fn delete_nonempty_dir_requires_recursive() {
        let base = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(base.path().join("d")).await.unwrap();
        tokio::fs::write(base.path().join("d/f.txt"), b"x").await.unwrap();

        // Without recursive → error (409).
        let err = do_delete(base.path(), "d", false).await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);

        // With recursive → removed.
        do_delete(base.path(), "d", true).await.unwrap();
        assert!(!base.path().join("d").exists());
    }

    #[tokio::test]
    async fn delete_refuses_workspace_root() {
        let base = tempfile::tempdir().unwrap();
        let err = do_delete(base.path(), ".", true).await.unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn mkdir_creates_nested() {
        let base = tempfile::tempdir().unwrap();
        do_mkdir(base.path(), "a/b/c").await.unwrap();
        assert!(base.path().join("a/b/c").is_dir());
        // Idempotent.
        do_mkdir(base.path(), "a/b/c").await.unwrap();
    }

    #[tokio::test]
    async fn rename_moves_file_refuses_collision() {
        let base = tempfile::tempdir().unwrap();
        tokio::fs::write(base.path().join("old.md"), b"x").await.unwrap();
        do_rename(base.path(), "old.md", "new.md").await.unwrap();
        assert!(base.path().join("new.md").exists());
        assert!(!base.path().join("old.md").exists());

        tokio::fs::write(base.path().join("a.md"), b"a").await.unwrap();
        let err = do_rename(base.path(), "a.md", "new.md").await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT, "collision must 409");
    }

    #[tokio::test]
    async fn upload_sanitizes_basename_and_writes() {
        let base = tempfile::tempdir().unwrap();
        // Path components in filename are stripped to basename.
        let rel = save_upload(base.path(), "sub", "../../evil.png", b"data").await.unwrap();
        assert_eq!(rel, "sub/evil.png");
        assert_eq!(tokio::fs::read(base.path().join("sub/evil.png")).await.unwrap(), b"data");
    }

    #[tokio::test]
    async fn upload_rejects_oversize() {
        let base = tempfile::tempdir().unwrap();
        let big = vec![0u8; MAX_UPLOAD_BYTES + 1];
        let err = save_upload(base.path(), "", "big.bin", &big).await.unwrap_err();
        assert_eq!(err.0, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn resolve_within_is_read_only_no_dir_creation() {
        let base = tempfile::tempdir().unwrap();
        let _ = resolve_within(base.path(), "newdir/sub/file.png").await;
        assert!(!base.path().join("newdir").exists(), "resolve must not create dirs on disk");
    }

    #[tokio::test]
    async fn resolve_within_rejects_traversal_for_missing_target() {
        let base = tempfile::tempdir().unwrap();
        assert!(resolve_within(base.path(), "../../escape/x.png").await.is_err(), "traversal on a non-existent target must still be denied");
    }

    #[tokio::test]
    async fn resolve_within_accepts_missing_nested_inside() {
        let base = tempfile::tempdir().unwrap();
        let (b, t) = resolve_within(base.path(), "a/b/file.md").await.unwrap();
        assert!(t.starts_with(&b), "valid non-existent nested path resolves inside base");
        assert!(!base.path().join("a").exists(), "still no dir creation");
    }

    #[tokio::test]
    async fn resolve_within_rejects_dotdot_bypass_payload() {
        // Regression: "subdir/../.." ends in `..`; the lexical fallback used to pass
        // the component-level starts_with check on Linux. Must be denied everywhere.
        let base = tempfile::tempdir().unwrap();
        assert!(resolve_within(base.path(), "subdir/../..").await.is_err());
    }

    #[tokio::test]
    async fn resolve_within_rejects_any_dotdot_component() {
        let base = tempfile::tempdir().unwrap();
        assert!(resolve_within(base.path(), "a/../b").await.is_err());
        assert!(resolve_within(base.path(), "..").await.is_err());
        assert!(resolve_within(base.path(), "ok/inside.md").await.is_ok());
    }

    /// Binary-by-extension files must NOT be read; size comes from metadata.
    #[tokio::test]
    async fn build_response_binary_stat_only_no_content_read() {
        let base = tempfile::tempdir().unwrap();
        // Write a 5-byte .mp4 file; the handler must return size=5 via metadata,
        // never reading bytes into RAM (we can't intercept the syscall, but we
        // verify the correct size is reported and is_binary=true).
        tokio::fs::write(base.path().join("clip.mp4"), b"12345").await.unwrap();
        let v = build_file_response(base.path(), "clip.mp4", &[9u8; 32], 3600).await.unwrap();
        assert_eq!(v["is_binary"], true);
        assert_eq!(v["size"], 5u64);
        let url = v["url"].as_str().unwrap();
        assert!(url.starts_with("/workspace-files/clip.mp4?sig="), "got {url}");
        // No "content" field.
        assert!(v.get("content").is_none());
    }

    /// A non-binary-extension file larger than MAX_TEXT_READ_BYTES must be
    /// served as binary (signed URL with size from metadata) without reading it.
    #[tokio::test]
    async fn build_response_oversized_text_ext_treated_as_binary() {
        let base = tempfile::tempdir().unwrap();
        // Create a file that exceeds MAX_TEXT_READ_BYTES using a real temp file,
        // but we verify the cap logic by temporarily creating exactly
        // MAX_TEXT_READ_BYTES + 1 bytes.  Writing 50 MiB + 1 bytes is possible
        // but slow; instead we rely on the metadata branch: write a small file,
        // then use std::fs to truncate/extend it to the cap + 1.
        let path = base.path().join("big.log");
        {
            let f = std::fs::File::create(&path).unwrap();
            f.set_len(MAX_TEXT_READ_BYTES + 1).unwrap();
        }
        let meta = tokio::fs::metadata(&path).await.unwrap();
        assert_eq!(meta.len(), MAX_TEXT_READ_BYTES + 1);

        let v = build_file_response(base.path(), "big.log", &[11u8; 32], 3600).await.unwrap();
        assert_eq!(v["is_binary"], true, "oversized non-binary-ext file should be treated as binary");
        assert_eq!(v["size"], MAX_TEXT_READ_BYTES + 1);
        assert!(v.get("content").is_none());
    }

    #[tokio::test]
    async fn sign_map_skips_external_and_missing() {
        let base = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(base.path().join("note/images")).await.unwrap();
        tokio::fs::write(base.path().join("note/images/x.png"), b"x").await.unwrap();

        let paths = vec![
            "note/images/x.png".to_string(),
            "note/images/missing.png".to_string(),
            "../../etc/passwd".to_string(),
        ];
        let m = build_sign_map(base.path(), &paths, &[3u8; 32], 3600).await;

        assert!(m.contains_key("note/images/x.png"));
        assert!(!m.contains_key("note/images/missing.png"), "missing skipped");
        assert!(!m.contains_key("../../etc/passwd"), "external skipped");
        let url = m["note/images/x.png"].as_str().unwrap();
        assert!(url.starts_with("/workspace-files/note/images/x.png?sig="), "got {url}");
    }
}
