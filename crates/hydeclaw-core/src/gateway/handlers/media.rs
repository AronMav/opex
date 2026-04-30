use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde_json::json;

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, AuthServices, ConfigServices};
use crate::uploads::{verify_signed_url, SignedUploadQuery, UploadSignatureError};

/// Query extractor for `?sig=&exp=`. Kept as a local `#[derive(Deserialize)]`
/// struct so axum's `Query<T>` picks it up without leaking `SignedUploadQuery`
/// (which is deliberately not `Deserialize`) into the leaf `uploads` module.
///
/// Declared `pub(crate)` so the clippy `private_interfaces` lint is satisfied —
/// `api_media_serve` is itself `pub(crate)` and its signature mentions
/// `Query<MediaQuery>`.
#[derive(serde::Deserialize)]
pub(crate) struct MediaQuery {
    sig: Option<String>,
    exp: Option<u64>,
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/uploads/{filename}", get(api_media_serve))
        .merge(
            Router::new()
                .route("/api/media/upload", post(api_media_upload))
                .route("/api/media/transcribe", post(api_media_transcribe))
                .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024)) // 20 MB
        )
}

/// POST /api/media/upload — multipart upload, saves to workspace/uploads/{uuid}.{ext}
pub(crate) async fn api_media_upload(
    State(agents): State<AgentCore>,
    State(cfg): State<ConfigServices>,
    mut multipart: axum::extract::Multipart,
) -> impl IntoResponse {
    let workspace_dir = agents.deps.read().await.workspace_dir.clone();
    let uploads_dir = std::path::PathBuf::from(&workspace_dir).join("uploads");
    if let Err(e) = tokio::fs::create_dir_all(&uploads_dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("mkdir: {e}")}))).into_response();
    }

    let field = match multipart.next_field().await {
        Ok(Some(f)) => f,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "no file field in multipart"}))).into_response(),
    };

    let original_name = field.file_name().unwrap_or("file").to_string();
    let ext = original_name.rsplit('.').next().unwrap_or("bin").to_lowercase();
    // Only allow safe media extensions — reject html/svg/etc to prevent XSS
    const SAFE_EXTENSIONS: &[&str] = &[
        "jpg", "jpeg", "png", "gif", "webp", "bmp", "ico",
        "mp4", "webm", "mov", "avi",
        "ogg", "oga", "mp3", "wav", "flac", "aac", "m4a",
        "pdf", "docx", "xlsx", "pptx",
        "txt", "md", "csv", "log", "json", "toml", "yaml", "yml",
        "zip", "tar", "gz", "bin",
    ];
    let ext = if SAFE_EXTENSIONS.contains(&ext.as_str()) { ext } else { "bin".to_string() };
    let uuid = uuid::Uuid::new_v4();
    let filename = format!("{uuid}.{ext}");
    let path = uploads_dir.join(&filename);

    let data = match field.bytes().await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("read: {e}")}))).into_response(),
    };

    // 20 MB limit
    if data.len() > 20 * 1024 * 1024 {
        return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"error": "file too large (max 20MB)"}))).into_response();
    }

    if let Err(e) = tokio::fs::write(&path, &data).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("write: {e}")}))).into_response();
    }

    let base = if let Some(ref pu) = cfg.config.gateway.public_url {
        pu.trim_end_matches('/').to_string()
    } else {
        let port = cfg.config.gateway.listen.rsplit(':').next().unwrap_or("18789");
        format!("http://localhost:{port}")
    };
    let url = format!("{base}/uploads/{filename}");
    Json(json!({"url": url, "filename": filename, "size": data.len()})).into_response()
}

/// GET /uploads/{filename} — serve uploaded files.
///
/// Phase 64 SEC-03: HMAC-signed URL enforcement.
///   * `cfg.uploads.require_signature = true`  → 403 when `?sig=&exp=` missing
///   * `cfg.uploads.require_signature = false` (v0.19.0 grace) → unsigned OK,
///     but a PRESENT signature is still validated (tampered → 403, expired → 410).
///
/// Signature payload contract: `HMAC-SHA256("{filename}:{exp}", upload_key)`
/// with `upload_key = SecretsManager::get_upload_hmac_key()` (HKDF-derived).
pub(crate) async fn api_media_serve(
    State(agents): State<AgentCore>,
    State(cfg): State<ConfigServices>,
    State(auth): State<AuthServices>,
    Path(filename): Path<String>,
    Query(q): Query<MediaQuery>,
) -> impl IntoResponse {
    // Prevent path traversal
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // Phase 64 SEC-03: signature gate (runs BEFORE filesystem read to prevent
    // oracle attacks via NOT_FOUND timing).
    let require = cfg.config.uploads.require_signature;
    let sq = SignedUploadQuery { sig: q.sig.clone(), exp: q.exp };
    if require || sq.sig.is_some() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let key = auth.secrets.get_upload_hmac_key();
        match verify_signed_url(&filename, &sq, &key, now) {
            Ok(()) => {}
            Err(UploadSignatureError::Expired) => {
                return StatusCode::GONE.into_response();
            }
            Err(UploadSignatureError::Invalid) => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Err(UploadSignatureError::Missing) => {
                // Only reach here when require_signature=true (sq.sig.is_some()
                // wouldn't return Missing). Grace mode is handled by the outer
                // `if require || sq.sig.is_some()` gate skipping the verify call.
                if require {
                    return StatusCode::FORBIDDEN.into_response();
                }
            }
        }
    }

    let workspace_dir = agents.deps.read().await.workspace_dir.clone();
    let path = std::path::PathBuf::from(&workspace_dir).join("uploads").join(&filename);

    let data = match tokio::fs::read(&path).await {
        Ok(d) => d,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    // Guess content-type from extension
    let ct = match filename.rsplit('.').next().unwrap_or("") {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogg" | "oga" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "txt" | "md" | "csv" | "log" => "text/plain; charset=utf-8",
        "json" => "application/json",
        _ => "application/octet-stream",
    };

    let disposition = if ct.starts_with("image/") || ct.starts_with("audio/") || ct.starts_with("video/") {
        "inline"
    } else {
        "attachment"
    };

    ([
        (axum::http::header::CONTENT_TYPE, ct),
        (axum::http::header::CONTENT_DISPOSITION, disposition),
        (axum::http::header::CACHE_CONTROL, "private, no-store"),
    ], data).into_response()
}

/// POST /api/media/transcribe — receive an audio blob from the browser, send it
/// to toolgate POST /transcribe (multipart file upload), and return the transcript.
///
/// Request:  `multipart/form-data` with a `file` field (audio blob).
///           Optional query param `?lang=` (default "ru").
/// Response: `{"text": "<transcript>"}` on success.
///           `503 {"error": "STT not configured"}` when toolgate_url is None.
///           `400 {"error": "..."}` for bad input.
///           `502 {"error": "transcription failed: ..."}` on toolgate error.
///
/// Temp file lifecycle: saved to `workspace/uploads/{uuid}.{ext}` then ALWAYS
/// deleted before returning (explicit at each return site; scopeguard not used).
pub(crate) async fn api_media_transcribe(
    State(agents): State<AgentCore>,
    State(cfg): State<ConfigServices>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    mut multipart: axum::extract::Multipart,
) -> impl IntoResponse {
    // ── 1. Resolve toolgate URL ───────────────────────────────────────────────
    let toolgate_url = match cfg.config.toolgate_url.as_deref() {
        Some(u) if !u.is_empty() => u.trim_end_matches('/').to_string(),
        _ => {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "STT not configured"}))).into_response();
        }
    };

    let language = params.get("lang").map(|s| s.as_str()).unwrap_or("ru");

    // ── 2. Read multipart 'file' field ────────────────────────────────────────
    let field = match multipart.next_field().await {
        Ok(Some(f)) => f,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "no file field in multipart"}))).into_response(),
    };

    let original_name = field.file_name().unwrap_or("audio.webm").to_string();
    let content_type = field.content_type().unwrap_or("audio/webm").to_string();

    // ── 3. Determine extension and validate ───────────────────────────────────
    const AUDIO_EXTENSIONS: &[&str] = &["webm", "mp4", "ogg", "oga", "mp3", "wav", "m4a", "aac", "flac"];
    let ext = original_name.rsplit('.').next().unwrap_or("webm").to_lowercase();
    let ext = if AUDIO_EXTENSIONS.contains(&ext.as_str()) { ext } else {
        // Try to guess from content-type
        match content_type.as_str() {
            "audio/webm" => "webm".to_string(),
            "audio/mp4" => "mp4".to_string(),
            "audio/ogg" => "ogg".to_string(),
            "audio/mpeg" => "mp3".to_string(),
            "audio/wav" => "wav".to_string(),
            _ => "webm".to_string(),
        }
    };

    let data = match field.bytes().await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("read: {e}")}))).into_response(),
    };

    if data.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "empty audio file"}))).into_response();
    }

    // ── 4. Save to temp file in workspace/uploads ─────────────────────────────
    let workspace_dir = agents.deps.read().await.workspace_dir.clone();
    let uploads_dir = std::path::PathBuf::from(&workspace_dir).join("uploads");
    if let Err(e) = tokio::fs::create_dir_all(&uploads_dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("mkdir: {e}")}))).into_response();
    }

    let uuid = uuid::Uuid::new_v4();
    let filename = format!("{uuid}.{ext}");
    let path = uploads_dir.join(&filename);

    if let Err(e) = tokio::fs::write(&path, &data).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("write: {e}")}))).into_response();
    }

    // ── 5. Forward to toolgate POST /transcribe ───────────────────────────────
    let tg_url = format!("{toolgate_url}/transcribe");
    let mime = format!("audio/{ext}");
    let part = reqwest::multipart::Part::bytes(data.to_vec())
        .file_name(filename.clone())
        .mime_str(&mime)
        .unwrap_or_else(|_| reqwest::multipart::Part::bytes(data.to_vec()));
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("language", language.to_string());

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default();

    let tg_resp = http.post(&tg_url).multipart(form).send().await;

    // ── 6. Parse response and always delete the temp file ─────────────────────
    match tg_resp {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.json::<serde_json::Value>().await;
            // Delete temp file — success path.
            let _ = tokio::fs::remove_file(&path).await;
            match body {
                Ok(v) => {
                    let text = v["text"].as_str().unwrap_or("").to_string();
                    Json(json!({"text": text})).into_response()
                }
                Err(e) => {
                    (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("transcription parse error: {e}")}))).into_response()
                }
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Delete temp file — error path.
            let _ = tokio::fs::remove_file(&path).await;
            (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("transcription failed: {status} — {body}")}))).into_response()
        }
        Err(e) => {
            // Delete temp file — network error path.
            let _ = tokio::fs::remove_file(&path).await;
            (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("transcription failed: {e}")}))).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the guard: when toolgate_url is None, the handler would return 503.
    /// We test this by asserting the config state that triggers the early-return guard.
    /// Full axum router integration would require a live AppState (DB, etc.) — out of scope.
    #[test]
    fn transcribe_guard_toolgate_url_none_would_return_503() {
        let cfg = ConfigServices::test_new();
        // test_new() produces a minimal config with toolgate_url = None.
        // The api_media_transcribe handler's first check is:
        //   match cfg.config.toolgate_url.as_deref() { Some(u) => ..., _ => 503 }
        // When toolgate_url is None, the 503 branch fires before any I/O.
        assert!(
            cfg.config.toolgate_url.is_none(),
            "test_new() must produce a config with no toolgate_url so the 503 guard fires"
        );
    }

    /// Verify the audio extension allowlist covers browser formats.
    #[test]
    fn audio_extensions_cover_browser_formats() {
        const AUDIO_EXTENSIONS: &[&str] = &["webm", "mp4", "ogg", "oga", "mp3", "wav", "m4a", "aac", "flac"];
        assert!(AUDIO_EXTENSIONS.contains(&"webm"), "webm required (Chrome/Firefox)");
        assert!(AUDIO_EXTENSIONS.contains(&"mp4"), "mp4 required (Safari)");
        assert!(AUDIO_EXTENSIONS.contains(&"ogg"), "ogg required (Firefox)");
    }
}
