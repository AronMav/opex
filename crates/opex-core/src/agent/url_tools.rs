//! URL extraction and content helpers — standalone functions used by the engine.

use crate::agent::handler_registry::match_buttons;
use crate::agent::fse::get_enabled_allowlist;

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
/// For each attachment, resolves the available file handlers via `match_buttons`
/// and injects a structured hint listing them with `handler_id` + label +
/// description. The model can then call `file_handler(action="run")` directly
/// when the user's text already indicates what to do (e.g. "расшифруй" →
/// `transcribe`), or offer a brief choice when it doesn't — without the
/// extra round-trip of `action="list"`.
///
/// The model MUST NOT try to read the bytes itself (it has no multimodal
/// channel — `Message.content` is `String`); all media is processed
/// out-of-band through toolgate handlers.
pub(crate) async fn enrich_with_attachments(
    text: &mut String,
    attachments: &[opex_types::MediaAttachment],
    handlers: &crate::agent::handler_registry::HandlerRegistry,
    db: &sqlx::PgPool,
    lang: &str,
) {
    use opex_types::MediaType;

    // Debug: log what we received
    tracing::info!(
        attachment_count = attachments.len(),
        "enrich_with_attachments: processing attachments"
    );
    for (i, att) in attachments.iter().enumerate() {
        tracing::info!(
            idx = i,
            url = %att.url,
            media_type = ?att.media_type,
            file_name = ?att.file_name,
            mime_type = ?att.mime_type,
            "enrich_with_attachments: attachment"
        );
    }

    let manifests = handlers.manifests().await;
    if manifests.is_empty() {
        // Toolgate not reachable or no handlers configured — fall back to
        // the generic hint so the model at least knows a file was sent.
        for att in attachments {
            let kind = match att.media_type {
                MediaType::Image => "изображение",
                MediaType::Audio => "голосовое сообщение",
                MediaType::Video => "видео",
                MediaType::Document => "документ",
            };
            let name = att.file_name.as_deref().unwrap_or("");
            let extra = if name.is_empty() { String::new() } else { format!(" «{name}»") };
            let hint = match extract_upload_id(&att.url) {
                Some(id) => format!(
                    "[Пользователь прислал {kind}{extra} (upload_id: {id}). \
                     Вызови file_handler с action=\"list\" и upload_id=\"{id}\" \
                     для просмотра доступных обработчиков.]"
                ),
                None => format!("[User sent a {kind}{extra}: {}]", att.url),
            };
            push_hint(text, &hint);
        }
        return;
    }

    let enabled = get_enabled_allowlist(db).await;

    for att in attachments {
        let upload_id = match extract_upload_id(&att.url) {
            Some(id) => id,
            None => {
                // Non-upload URL (e.g. direct hotlink) — simple hint, no handler menu.
                let kind = match att.media_type {
                    MediaType::Image => "image",
                    MediaType::Audio => "audio",
                    MediaType::Video => "video",
                    MediaType::Document => "document",
                };
                let name = att.file_name.as_deref().unwrap_or("");
                let extra = if name.is_empty() { String::new() } else { format!(" «{name}»") };
                push_hint(text, &format!("[User sent a {kind}{extra}: {}]", att.url));
                continue;
            }
        };

        // Resolve the upload row to get MIME + size for match_buttons.
        let upload_uuid = match uuid::Uuid::parse_str(upload_id) {
            Ok(u) => u,
            Err(_) => {
                push_hint(text, &format!(
                    "[Пользователь прислал файл (upload_id: {upload_id}). \
                     Вызови file_handler с action=\"list\" и upload_id=\"{upload_id}\".]"
                ));
                continue;
            }
        };
        let row = match crate::db::uploads::get_by_id(db, upload_uuid).await {
            Ok(Some(r)) => r,
            _ => {
                push_hint(text, &format!(
                    "[Пользователь прислал файл (upload_id: {upload_id}). \
                     Вызови file_handler с action=\"list\" и upload_id=\"{upload_id}\".]"
                ));
                continue;
            }
        };

        let mime = &row.mime;
        let size = u64::try_from(row.size_bytes).unwrap_or(0);
        let buttons = match_buttons(
            &manifests, mime, size, &enabled, lang,
        );

        let kind = match att.media_type {
            MediaType::Image => "изображение",
            MediaType::Audio => "голосовое сообщение",
            MediaType::Video => "видео",
            MediaType::Document => "документ",
        };
        let name = att.file_name.as_deref().unwrap_or("");
        let extra = if name.is_empty() { String::new() } else { format!(" «{name}»") };

        if buttons.is_empty() {
            push_hint(text, &format!(
                "[Пользователь прислал {kind}{extra} (upload_id: {upload_id}). \
                 НЕ пытайся прочитать это сама — у тебя нет прямого доступа к байтам файла. \
                 Для этого типа файла нет доступных обработчиков.]"
            ));
            continue;
        }

        // Build a structured handler list the model can act on directly.
        let mut handler_lines = String::new();
        for b in &buttons {
            let desc = manifests
                .iter()
                .find(|m| m.id == b.id)
                .and_then(|m| m.descriptions.get(lang).or_else(|| m.descriptions.get("en")))
                .cloned()
                .unwrap_or_default();
            handler_lines.push_str(&format!("  • {} ({})", b.id, b.label));
            if !desc.is_empty() {
                handler_lines.push_str(&format!(" — {desc}"));
            }
            handler_lines.push('\n');
        }

        push_hint(text, &format!(
            "[Пользователь прислал {kind}{extra} (upload_id: {upload_id}, mime: {mime}, size: {size} bytes).\n\
             НЕ пытайся прочитать это сама — у тебя нет прямого доступа к байтам файла.\n\
             Доступные обработчики:\n\
             {handler_lines}\
             Если пользователь уже указал в сообщении, что делать с файлом — вызови \
             file_handler с action=\"run\", upload_id=\"{upload_id}\" и подходящим handler_id. \
             Если не указал — кратко предложи выбрать из списка выше и вызови \
             file_handler с action=\"run\" после выбора.]"
        ));
    }
}

fn push_hint(text: &mut String, hint: &str) {
    if text.is_empty() {
        *text = hint.to_string();
    } else {
        text.push('\n');
        text.push_str(hint);
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
    use crate::agent::handler_registry::HandlerRegistry;

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

    fn empty_registry() -> HandlerRegistry {
        HandlerRegistry::new("http://127.0.0.1:1".to_string(), reqwest::Client::new())
    }

    fn test_db() -> sqlx::PgPool {
        sqlx::PgPool::connect_lazy("postgres://invalid").expect("lazy pool")
    }

    #[tokio::test]
    async fn enrich_with_attachments_empty_no_change() {
        let mut text = "hello".to_string();
        enrich_with_attachments(&mut text, &[], &empty_registry(), &test_db(), "ru").await;
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn enrich_with_attachments_image_without_upload_id_falls_back_to_url() {
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
        enrich_with_attachments(&mut text, &[att], &empty_registry(), &test_db(), "ru").await;
        assert!(
            text.contains("https://example.com/img.jpg"),
            "image url survives in hint: {text}"
        );
        assert!(
            !text.contains("file_handler"),
            "no upload_id → no file_handler menu: {text}"
        );
    }

    #[tokio::test]
    async fn enrich_with_attachments_empty_manifests_falls_back_to_list_hint() {
        // No handlers configured (toolgate unreachable) → generic hint with
        // action="list" so the model can still call file_handler.
        let mut text = String::new();
        let att = opex_types::MediaAttachment {
            url: "https://host/api/uploads/44444444-4444-4444-8444-444444444444?sig=x".to_string(),
            media_type: opex_types::MediaType::Image,
            file_name: Some("photo.png".to_string()),
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att], &empty_registry(), &test_db(), "ru").await;
        // With empty manifests, the fallback hint says action="list".
        assert!(
            text.contains("file_handler"),
            "fallback hint points at file_handler: {text}"
        );
        assert!(
            text.contains("44444444-4444-4444-8444-444444444444"),
            "hint carries the upload_id: {text}"
        );
    }

    #[tokio::test]
    async fn enrich_with_attachments_multiple_joined_with_newline() {
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
        enrich_with_attachments(&mut text, &[att1, att2], &empty_registry(), &test_db(), "ru").await;
        assert!(
            text.contains("https://example.com/img.jpg"),
            "image url survives: {text}"
        );
        assert!(
            text.contains("file_handler"),
            "audio hint points at file_handler: {text}"
        );
    }

    #[tokio::test]
    async fn enrich_with_attachments_non_upload_url_no_handler() {
        let mut text = String::new();
        let att = opex_types::MediaAttachment {
            url: "https://example.com/doc.pdf".to_string(),
            media_type: opex_types::MediaType::Document,
            file_name: Some("report.pdf".to_string()),
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att], &empty_registry(), &test_db(), "ru").await;
        assert!(
            text.contains("report.pdf"),
            "document hint keeps the filename: {text}"
        );
        assert!(
            !text.contains("file_handler"),
            "non-upload URL → no file_handler menu: {text}"
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
