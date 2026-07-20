//! URL extraction and content helpers — standalone functions used by the engine.

/// Convert a public attachment URL to a localhost URL for internal Core downloads.
///
/// When `public_url` is configured (e.g. `https://192.168.1.85`), `att.url` contains
/// that host+scheme. Connecting to it from inside Core fails (TLS cert, CGNAT, etc.).
/// We always download from `http://localhost:{port}` instead, using just the path component.
// reviewed: offsets from find("/api/uploads/")/find("/uploads/") (ASCII) — char boundaries
#[allow(clippy::string_slice)]
pub(crate) fn uploads_local_url(att_url: &str, gateway_listen: &str) -> String {
    let port = gateway_listen
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(18789);
    // Extract path from URL. After the uploads-to-db migration the public
    // shape is "/api/uploads/{id}?sig=...&exp=...". Prefer the longer prefix
    // so `find()` doesn't match the inner "/uploads/" inside "/api/uploads/"
    // and strip the "/api" segment. Bare "/uploads/{filename}" is no longer
    // routable (the legacy filesystem-serving handler was removed); the
    // fallback exists so malformed legacy URLs at least resolve to a
    // deterministic 404 path instead of being passed verbatim to the
    // localhost downloader.
    let path = if let Some(idx) = att_url.find("/api/uploads/") {
        &att_url[idx..]
    } else if let Some(idx) = att_url.find("/uploads/") {
        &att_url[idx..]
    } else {
        att_url
    };
    format!("http://localhost:{port}{path}")
}

/// Extract the upload UUID from an attachment URL shaped like
/// `…/api/uploads/{uuid}?sig=…`. Returns `None` when the URL isn't an uploads
/// link — used to point the model's `file_handler` tool at an uploaded file.
// reviewed: split on ASCII markers, byte-length check — char boundaries safe
pub(crate) fn extract_upload_id(att_url: &str) -> Option<&str> {
    let rest = att_url.split("/api/uploads/").nth(1)?;
    let id = rest.split(['?', '/', '&']).next()?;
    if id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        Some(id)
    } else {
        None
    }
}

/// Append media attachment hints to the enriched text for the LLM.
///
/// For audio/video/image/document attachments the hint points the model at the
/// `file_handler` tool (action=list to fetch the applicable handlers, action=run
/// to execute the user's choice) — the same model-driven menu used for links.
/// The model MUST NOT try to read the bytes itself (it has no multimodal
/// channel — `Message.content` is `String`); all media is processed out-of-band
/// through toolgate handlers (`describe`, `transcribe`, `extract_document`,
/// `save`) or, for images, the `analyze_image` capability tool when a vision
/// provider is configured.
pub(crate) fn enrich_with_attachments(text: &mut String, attachments: &[opex_types::MediaAttachment]) {
    use opex_types::MediaType;
    // Model-driven handler menu for a handleable upload. Explicitly forbids
    // the model from attempting to "read" the bytes itself (which would emit
    // fake "this model does not support image input" errors on text-only LLMs).
    let menu = |kind: &str, extra: &str, url: &str| -> String {
        match extract_upload_id(url) {
            Some(id) => format!(
                "[Пользователь прислал {kind}{extra} (upload_id: {id}). НЕ пытайся прочитать \
                 это сама — у тебя нет прямого доступа к байтам файла. Вызови инструмент \
                 file_handler с action=\"list\" и upload_id=\"{id}\", покажи доступные \
                 обработчики и по выбору пользователя вызови file_handler с action=\"run\", \
                 тем же upload_id и выбранным handler_id.]"
            ),
            None => format!("[User sent a {kind}{extra}: {url}]"),
        }
    };
    for att in attachments {
        let hint = match att.media_type {
            // Images use the same menu hint — `describe` (vision) and `save`
            // are exposed by `file_handler` after the sync-execution fix.
            // If `analyze_image` is available as a capability tool the model
            // may pick that instead, but the hint must not assume it.
            MediaType::Image => {
                let name = att.file_name.as_deref().unwrap_or("image");
                menu("изображение", &format!(" «{name}»"), &att.url)
            }
            MediaType::Audio => menu("голосовое сообщение", "", &att.url),
            MediaType::Video => menu("видео", "", &att.url),
            MediaType::Document => {
                let name = att.file_name.as_deref().unwrap_or("file");
                menu("документ", &format!(" «{name}»"), &att.url)
            }
        };
        if text.is_empty() {
            *text = hint;
        } else {
            text.push('\n');
            text.push_str(&hint);
        }
    }
}

/// Extract URLs from text (deduplicated, order-preserving).
pub(crate) fn extract_urls(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.split_whitespace()
        .filter(|w| w.starts_with("http://") || w.starts_with("https://"))
        .map(|w| w.trim_matches(|c: char| ",.)]}>".contains(c)).to_string())
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_urls ─────────────────────────────────────────────────────────

    #[test]
    fn extract_urls_no_urls_returns_empty() {
        assert_eq!(extract_urls("hello world, no links here"), Vec::<String>::new());
    }

    #[test]
    fn extract_urls_single_http() {
        assert_eq!(extract_urls("visit http://example.com today"), vec!["http://example.com"]);
    }

    #[test]
    fn extract_urls_single_https() {
        assert_eq!(extract_urls("see https://rust-lang.org for details"), vec!["https://rust-lang.org"]);
    }

    #[test]
    fn extract_urls_deduplication() {
        let text = "https://example.com https://example.com https://other.com";
        let urls = extract_urls(text);
        assert_eq!(urls, vec!["https://example.com", "https://other.com"]);
    }

    #[test]
    fn extract_urls_trailing_punctuation_trimmed() {
        let text = "check https://example.com, and https://other.com.";
        let urls = extract_urls(text);
        assert_eq!(urls, vec!["https://example.com", "https://other.com"]);
    }

    // ── enrich_with_attachments ──────────────────────────────────────────────

    #[test]
    fn enrich_with_attachments_empty_no_change() {
        let mut text = "hello".to_string();
        enrich_with_attachments(&mut text, &[]);
        assert_eq!(text, "hello");
    }

    #[test]
    fn enrich_with_attachments_image_without_upload_id_falls_back_to_url() {
        // An image without an /api/uploads/{id} URL can't be routed through
        // file_handler (no upload_id to pass) — keep the bare inline note.
        let mut text = String::new();
        let att = opex_types::MediaAttachment {
            url: "https://example.com/img.jpg".to_string(),
            media_type: opex_types::MediaType::Image,
            file_name: None,
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att]);
        assert!(
            text.contains("https://example.com/img.jpg"),
            "image url survives in hint: {text}"
        );
        assert!(
            !text.contains("file_handler"),
            "no upload_id → no file_handler menu: {text}"
        );
    }

    #[test]
    fn enrich_with_attachments_image_with_upload_id_routes_via_file_handler() {
        // The fix for the "this model does not support image input" leak: an
        // image upload MUST point at file_handler so the model does not try
        // to read the bytes itself.
        let mut text = String::new();
        let att = opex_types::MediaAttachment {
            url: "https://host/api/uploads/44444444-4444-4444-8444-444444444444?sig=x".to_string(),
            media_type: opex_types::MediaType::Image,
            file_name: Some("photo.png".to_string()),
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att]);
        assert!(text.contains("file_handler"), "image hint points at file_handler: {text}");
        assert!(
            text.contains("44444444-4444-4444-8444-444444444444"),
            "image hint carries the upload_id: {text}"
        );
        assert!(text.contains("photo.png"), "image hint keeps the filename: {text}");
        assert!(
            text.contains("НЕ пытайся прочитать"),
            "image hint forbids the model from reading bytes itself: {text}"
        );
    }

    #[test]
    fn enrich_with_attachments_multiple_joined_with_newline() {
        let mut text = "look at this".to_string();
        let att1 = opex_types::MediaAttachment {
            url: "https://example.com/img.jpg".to_string(),
            media_type: opex_types::MediaType::Image,
            file_name: None,
            mime_type: None,
            file_size: None,
        };
        let att2 = opex_types::MediaAttachment {
            url: "https://host/api/uploads/11111111-1111-4111-8111-111111111111?sig=x&exp=1".to_string(),
            media_type: opex_types::MediaType::Audio,
            file_name: None,
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att1, att2]);
        assert!(
            text.contains("https://example.com/img.jpg"),
            "image url survives: {text}"
        );
        // Audio upload → file_handler menu with the extracted upload_id.
        assert!(text.contains("file_handler"), "audio hint points at file_handler: {text}");
        assert!(
            text.contains("11111111-1111-4111-8111-111111111111"),
            "audio hint carries the upload_id: {text}"
        );
    }

    #[test]
    fn enrich_with_attachments_document_with_filename() {
        let mut text = String::new();
        let att = opex_types::MediaAttachment {
            url: "https://host/api/uploads/22222222-2222-4222-8222-222222222222?sig=x".to_string(),
            media_type: opex_types::MediaType::Document,
            file_name: Some("report.pdf".to_string()),
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att]);
        assert!(text.contains("file_handler"), "document hint points at file_handler: {text}");
        assert!(text.contains("report.pdf"), "document hint keeps the filename: {text}");
        assert!(
            text.contains("22222222-2222-4222-8222-222222222222"),
            "document hint carries the upload_id: {text}"
        );
    }

    #[test]
    fn extract_upload_id_parses_uploads_url() {
        assert_eq!(
            extract_upload_id("https://h/api/uploads/33333333-3333-4333-8333-333333333333?sig=a&exp=b"),
            Some("33333333-3333-4333-8333-333333333333")
        );
        assert_eq!(extract_upload_id("https://example.com/not-an-upload.mp4"), None);
    }
}
