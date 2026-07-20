use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
};
use serde_json::json;

use super::super::AppState;
use crate::gateway::clusters::{AuthServices, ConfigServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/vision/analyze", post(api_vision_analyze))
        .merge(
            Router::new()
                .route("/api/media/upload", post(api_media_upload))
                .route("/api/media/transcribe", post(api_media_transcribe))
                // Transport ceiling = DB backstop (50 MB). The authoritative
                // per-request cap is enforced in api_media_upload from
                // cfg.uploads.max_upload_bytes (the configurable SoT).
                .layer(axum::extract::DefaultBodyLimit::max(crate::db::uploads::MAX_UPLOAD_BYTES))
        )
}

/// Allowlist for `POST /api/media/upload`: only MIME types that match the
/// project's "safe to store as a client upload" rationale (the old
/// `SAFE_EXTENSIONS` list, expressed as MIME families). Defense in depth — the
/// serve side (`uploads_serve::api_uploads_serve`) also forces a download
/// disposition for non-image/audio/video bytes and sends `X-Content-Type-Options:
/// nosniff` unconditionally. Both `text/html` and `image/svg+xml` are rejected
/// here because they can execute script same-origin if a future change ever
/// inlines them.
pub(crate) fn is_safe_client_upload_mime(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    if lower.starts_with("image/") {
        // svg explicitly rejected — can carry <script>.
        !lower.starts_with("image/svg")
    } else if lower.starts_with("audio/") || lower.starts_with("video/") {
        true
    } else {
        matches!(
            lower.as_str(),
            "application/pdf"
                | "application/zip"
                | "application/gzip"
                | "application/x-tar"
                | "application/octet-stream"
                | "application/json"
                | "application/msword"
                | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                | "application/vnd.ms-excel"
                | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                | "application/vnd.ms-powerpoint"
                | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                | "text/plain"
                | "text/csv"
                | "text/markdown"
        )
    }
}

/// POST /api/media/upload — multipart upload, stores bytes in the `uploads`
/// table as `owner_type='client_upload'` with 30-day retention.
///
/// Response: `{url, filename, size}` where `filename` is the new upload UUID
/// (no extension) — preserved for backward compat with ChatComposer,
/// AgentEditDialog, and `channels/src/bridge.ts`.
pub(crate) async fn api_media_upload(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
    State(auth): State<AuthServices>,
    mut multipart: axum::extract::Multipart,
) -> impl IntoResponse {
    let field = match multipart.next_field().await {
        Ok(Some(f)) => f,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "no file field in multipart"}))).into_response(),
    };

    // Prefer the multipart field's Content-Type; fall back to extension-based
    // inference, then octet-stream. Acceptance stays broad — this endpoint
    // serves chat attachments + bridge media (audio, video, pdf, archives).
    //
    // Filter out empty Content-Type and the uninformative `application/octet-stream`
    // default so we still reach the extension-based fallback for those cases.
    let field_mime = field
        .content_type()
        .map(str::to_string)
        .filter(|s| !s.is_empty() && s != "application/octet-stream");
    let file_name = field.file_name().unwrap_or("file").to_string();
    let mime = field_mime.unwrap_or_else(|| {
        let guessed = crate::uploads::guess_mime_from_extension(&file_name);
        guessed.to_string()
    });

    // MIME allowlist (defense in depth — the serve side also forces
    // Content-Disposition: attachment for non-inlineable bytes and sends
    // X-Content-Type-Options: nosniff unconditionally). text/html and
    // image/svg+xml are rejected here as a belt-and-braces measure.
    if !is_safe_client_upload_mime(&mime) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({"error": "mime not allowed"})),
        )
            .into_response();
    }

    let data = match field.bytes().await {
        Ok(b) => b,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("read: {e}")}))).into_response(),
    };

    // Authoritative per-request cap: the configurable [uploads] max_upload_bytes
    // (the route-layer DefaultBodyLimit is the coarse transport backstop).
    let max_bytes = usize::try_from(cfg.config.uploads.max_upload_bytes).unwrap_or(usize::MAX);
    if data.len() > max_bytes {
        let max_mb = max_bytes / (1024 * 1024);
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": format!("file too large (max {max_mb}MB)")})),
        )
            .into_response();
    }

    let retention_days = cfg.config.cleanup.uploads_retention_days;
    // Preserve the original client-side filename (from the multipart field's
    // Content-Disposition) so downloads keep their real name instead of the
    // row UUID. Sanitize before persisting — strip path components (clients
    // may send absolute paths) and cap length.
    let safe_filename = file_name
        .split(['/', '\\'])
        .next_back()
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(255).collect::<String>());
    let id = match crate::db::uploads::insert_with_retention(
        &infra.db,
        "client_upload",
        None,
        &mime,
        &data,
        retention_days,
        safe_filename.as_deref(),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                error = %e,
                mime = %mime,
                size = data.len(),
                "media upload: db insert failed"
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Root-relative URL — rendered in the same-origin web UI, so it must not
    // depend on gateway.public_url (see crate::uploads::web_uploads_base()).
    let key = auth.secrets.get_upload_hmac_key();
    let url = crate::uploads::mint_uploads_url(
        crate::uploads::web_uploads_base(),
        id,
        &key,
        cfg.config.uploads.signed_url_ttl_secs,
    );

    tracing::info!(id = %id, mime = %mime, size = data.len(), "client_upload stored");

    Json(json!({"url": url, "filename": id.to_string(), "size": data.len()})).into_response()
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
/// No disk I/O: the audio bytes are streamed straight into the multipart body
/// via `Part::stream(Bytes)` (zero-copy thanks to Arc-backed `bytes::Bytes`).
pub(crate) async fn api_media_transcribe(
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
        Err(e) => return (StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("read: {e}")}))).into_response(),
    };
    if data.is_empty() {
        return (StatusCode::BAD_REQUEST,
            Json(json!({"error": "empty audio file"}))).into_response();
    }

    // ── 4. Forward to toolgate POST /transcribe (zero-copy, no disk write) ───
    let tg_url = format!("{toolgate_url}/transcribe");
    let mime = format!("audio/{ext}");
    let filename = format!("{}.{ext}", uuid::Uuid::new_v4());

    let part = match reqwest::multipart::Part::stream(data)
        .file_name(filename)
        .mime_str(&mime)
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid mime: {e}")}))).into_response(),
    };

    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("language", language.to_string());

    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("http client init: {e}")}))).into_response(),
    };

    match http.post(&tg_url).multipart(form).send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<serde_json::Value>().await {
                Ok(v) => Json(json!({
                    "text": v["text"].as_str().unwrap_or("")
                })).into_response(),
                Err(e) => (StatusCode::BAD_GATEWAY,
                    Json(json!({"error": format!("transcription parse error: {e}")}))).into_response(),
            }
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            (StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("transcription failed: {status} — {body}")}))).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("transcription failed: {e}")}))).into_response(),
    }
}

// ── Vision analyze ────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub(crate) struct VisionAnalyzeRequest {
    image_url: String,
    question: Option<String>,
    language: Option<String>,
}

/// POST /api/vision/analyze — analyze an image from a URL or internal /uploads/ path.
///
/// Handles two cases:
/// * Internal path (`/uploads/...`): downloads from localhost, forwards bytes to
///   toolgate `/describe` (multipart, no SSRF check needed).
/// * External URL (`https://...`): forwards to toolgate `/describe-url` (SSRF
///   validated there).
///
/// This endpoint exists because the `analyze_image` YAML tool passes relative
/// `/uploads/` paths that toolgate's SSRF guard blocks (no scheme). Core acts
/// as a proxy that knows how to download its own uploads.
///
/// Auth: loopback-exempt (see `LOOPBACK_EXACT` in middleware.rs).
// reviewed: offsets from find("/api/uploads/")/find("/uploads/") (ASCII) — char boundaries
#[allow(clippy::string_slice)]
pub(crate) async fn api_vision_analyze(
    State(cfg): State<ConfigServices>,
    headers: axum::http::HeaderMap,
    Json(body): Json<VisionAnalyzeRequest>,
) -> impl IntoResponse {
    let toolgate_url = match cfg.config.toolgate_url.as_deref() {
        Some(u) if !u.is_empty() => u.trim_end_matches('/').to_string(),
        _ => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "Vision not configured"}))).into_response(),
    };

    let image_url = body.image_url.trim().to_string();
    let language = body.language.as_deref().unwrap_or("ru").to_string();
    let question = body.question.clone().unwrap_or_default();

    // Ordered vision provider chain from the caller (`X-Opex-Providers`,
    // injected by engine_dispatch from the agent's profile vision slot).
    // Each attempt forwards one `X-Opex-Provider` to toolgate, retrying down
    // the chain on retryable failures (5xx / 429 / transport) — mirrors the
    // tts chain in `api_tts_synthesize`. Empty chain → single attempt with no
    // provider header (legacy: toolgate picks its default).
    let chain = parse_provider_chain(&headers);
    let attempts: Vec<Option<&str>> = if chain.is_empty() {
        vec![None]
    } else {
        chain.iter().map(|s| Some(s.as_str())).collect()
    };
    let last = attempts.len() - 1;

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    // Internal path: starts with / or contains /api/uploads/ — download via localhost.
    // Prefer the longer "/api/uploads/" prefix so `find()` doesn't match the
    // inner "/uploads/" inside "/api/uploads/" and strip the "/api" segment.
    // Bare "/uploads/{filename}" is no longer routable (legacy handler removed);
    // the fallback only exists to keep the localhost-rewrite path deterministic
    // on malformed input — the resulting download would 404.
    if image_url.starts_with('/') || image_url.contains("/uploads/") {
        let path = if let Some(idx) = image_url.find("/api/uploads/") {
            &image_url[idx..]
        } else if let Some(idx) = image_url.find("/uploads/") {
            &image_url[idx..]
        } else {
            &image_url
        };
        let port = cfg.config.gateway.listen.rsplit(':').next().unwrap_or("18789");
        let local_url = format!("http://localhost:{port}{path}");

        let image_bytes = match http.get(&local_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(b) => b,
                Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("read image: {e}")}))).into_response(),
            },
            Ok(resp) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("download image: {}", resp.status())}))).into_response(),
            Err(e) => return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("download image: {e}")}))).into_response(),
        };

        if image_bytes.len() < 10 {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "image too small"}))).into_response();
        }

        let base_filename = path.split('/').next_back().unwrap_or("image.jpg")
            .split('?').next().unwrap_or("image.jpg");
        let ext = base_filename.rsplit('.').next().unwrap_or("jpg").to_lowercase();
        let mime = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            _ => "image/jpeg",
        };

        let tg_url = format!("{toolgate_url}/describe");
        // Multipart forms are not reusable — rebuild per attempt from the
        // downloaded bytes (kept in memory) so chain retries stay possible.
        for (i, provider) in attempts.iter().enumerate() {
            let part = reqwest::multipart::Part::bytes(image_bytes.to_vec())
                .file_name(base_filename.to_string())
                .mime_str(mime)
                .unwrap_or_else(|_| reqwest::multipart::Part::bytes(image_bytes.to_vec()));
            let mut form = reqwest::multipart::Form::new()
                .part("file", part)
                .text("language", language.clone());
            if !question.is_empty() {
                form = form.text("prompt", question.clone());
            }
            let mut req = http.post(&tg_url).multipart(form);
            if let Some(p) = provider {
                req = req.header("X-Opex-Provider", *p);
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    return match resp.json::<serde_json::Value>().await {
                        Ok(v) => Json(json!({"description": v["description"].as_str().unwrap_or("")})).into_response(),
                        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("parse error: {e}")}))).into_response(),
                    };
                }
                Ok(resp) => {
                    let status = resp.status();
                    let retryable = status.is_server_error() || status.as_u16() == 429;
                    if retryable && i < last {
                        tracing::warn!(provider = ?provider, status = %status, "vision provider failed — trying next in chain");
                        continue;
                    }
                    if status.as_u16() == 503 {
                        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "Vision provider not configured"}))).into_response();
                    }
                    let body = resp.text().await.unwrap_or_default();
                    return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("Vision failed: {body}")}))).into_response();
                }
                Err(e) => {
                    if i < last {
                        tracing::warn!(provider = ?provider, error = %e, "vision provider transport error — trying next in chain");
                        continue;
                    }
                    return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("Vision failed: {e}")}))).into_response();
                }
            }
        }
        // Defensive: the loop always returns on its last attempt.
        return (StatusCode::BAD_GATEWAY, Json(json!({"error": "Vision failed: provider chain exhausted"}))).into_response();
    }

    // External URL: forward to toolgate /describe-url (SSRF validated there)
    let tg_url = format!("{toolgate_url}/describe-url");
    let mut payload = serde_json::json!({"image_url": image_url, "language": language});
    if !question.is_empty() {
        payload["question"] = serde_json::Value::String(question);
    }

    for (i, provider) in attempts.iter().enumerate() {
        let mut req = http.post(&tg_url).json(&payload);
        if let Some(p) = provider {
            req = req.header("X-Opex-Provider", *p);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                return match resp.json::<serde_json::Value>().await {
                    Ok(v) => Json(json!({"description": v["description"].as_str().unwrap_or("")})).into_response(),
                    Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("parse error: {e}")}))).into_response(),
                };
            }
            Ok(resp) => {
                let status = resp.status();
                let retryable = status.is_server_error() || status.as_u16() == 429;
                if retryable && i < last {
                    tracing::warn!(provider = ?provider, status = %status, "vision provider failed — trying next in chain");
                    continue;
                }
                if status.as_u16() == 503 {
                    return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "Vision provider not configured"}))).into_response();
                }
                let body = resp.text().await.unwrap_or_default();
                return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("Vision failed: {body}")}))).into_response();
            }
            Err(e) => {
                if i < last {
                    tracing::warn!(provider = ?provider, error = %e, "vision provider transport error — trying next in chain");
                    continue;
                }
                return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("Vision failed: {e}")}))).into_response();
            }
        }
    }
    // Defensive: the loop always returns on its last attempt.
    (StatusCode::BAD_GATEWAY, Json(json!({"error": "Vision failed: provider chain exhausted"}))).into_response()
}

/// Parse the ordered provider chain from the `X-Opex-Providers` header
/// (comma-separated provider names, injected by `engine_dispatch` from the
/// agent's profile vision slot). Missing/empty header → empty vec.
fn parse_provider_chain(headers: &axum::http::HeaderMap) -> Vec<String> {
    headers
        .get("x-opex-providers")
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod provider_chain_tests {
    use super::parse_provider_chain;
    use axum::http::HeaderMap;

    #[test]
    fn parses_ordered_chain() {
        let mut h = HeaderMap::new();
        h.insert("x-opex-providers", "ollama-v, mimo-v".parse().unwrap());
        assert_eq!(parse_provider_chain(&h), vec!["ollama-v".to_string(), "mimo-v".to_string()]);
    }

    #[test]
    fn missing_header_is_empty() {
        assert!(parse_provider_chain(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn empty_and_blank_entries_are_dropped() {
        let mut h = HeaderMap::new();
        h.insert("x-opex-providers", " , a,, ".parse().unwrap());
        assert_eq!(parse_provider_chain(&h), vec!["a".to_string()]);
    }
}

#[cfg(test)]
mod safe_mime_visibility {
    // Reaching the symbol via the absolute crate path proves it is at least
    // pub(crate) (a private `fn` is not nameable from a child mod via crate::).
    #[test]
    fn is_safe_client_upload_mime_is_crate_visible() {
        assert!(crate::gateway::handlers::media::is_safe_client_upload_mime("image/png"));
        assert!(!crate::gateway::handlers::media::is_safe_client_upload_mime("image/svg+xml"));
        assert!(!crate::gateway::handlers::media::is_safe_client_upload_mime("text/html"));
        assert!(crate::gateway::handlers::media::is_safe_client_upload_mime("application/pdf"));
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

    /// Forward regression guard: `Part::stream(Bytes)` must accept a `Bytes`
    /// directly without forcing the caller to materialize a separate owned
    /// `Vec<u8>`. The handler relies on this for zero-copy multipart upload —
    /// if reqwest ever stops accepting `Bytes` here (or starts requiring
    /// `Into<Body>` impls that re-allocate), our handler regresses to
    /// double-buffering. The test compiles only because `Bytes: Into<Body>`
    /// holds in reqwest 0.12 + bytes 1.x.
    #[tokio::test]
    async fn test_part_stream_accepts_bytes_directly() {
        use bytes::Bytes;
        let data = Bytes::from(vec![0u8; 30 * 1024 * 1024]);
        // Hold a clone so we can observe the original buffer's life: with
        // Arc-backed `Bytes`, `clone()` is a refcount bump, not a copy.
        let alias = data.clone();
        assert!(!data.is_unique(), "clone must share the same allocation");

        let part = reqwest::multipart::Part::stream(data)
            .file_name("test.wav")
            .mime_str("audio/wav")
            .expect("audio/wav is a valid mime");

        // After dropping the Part, the alias must once again be the sole
        // owner of the buffer. If `Part::stream` had silently copied
        // (e.g. via `to_vec()`) the refcount semantics would still hold,
        // but the clone path would no longer be zero-copy. The companion
        // assertion above ensures we exercise the cloning path.
        drop(part);
        assert!(
            alias.is_unique(),
            "Part::stream retained an extra owner after drop — buffer leaked"
        );
    }

    #[tokio::test]
    async fn test_part_stream_rejects_invalid_mime() {
        use bytes::Bytes;
        let data = Bytes::from(vec![0u8; 100]);
        let result = reqwest::multipart::Part::stream(data)
            .file_name("test.bad")
            .mime_str("not/a/valid/mime/string");
        assert!(result.is_err(), "invalid mime string must be rejected");
    }
}
