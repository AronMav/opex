//! URL extraction and content helpers — standalone functions used by the engine.

/// Convert a public attachment URL to a localhost URL for internal Core downloads.
///
/// When `public_url` is configured (e.g. `https://192.168.1.85`), `att.url` contains
/// that host+scheme. Connecting to it from inside Core fails (TLS cert, CGNAT, etc.).
/// We always download from `http://localhost:{port}` instead, using just the path component.
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

/// Append media attachment hints to the enriched text for LLM.
pub(crate) fn enrich_with_attachments(text: &mut String, attachments: &[hydeclaw_types::MediaAttachment]) {
    use hydeclaw_types::MediaType;
    for att in attachments {
        let hint = match att.media_type {
            MediaType::Image => format!("[User attached an image: {}]", att.url),
            MediaType::Audio => format!("[User sent a voice message: {}]", att.url),
            MediaType::Video => format!("[User sent a video: {}]", att.url),
            MediaType::Document => {
                let name = att.file_name.as_deref().unwrap_or("file");
                format!("[User attached a document \"{}\": {}]", name, att.url)
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
    fn enrich_with_attachments_single_image() {
        let mut text = String::new();
        let att = hydeclaw_types::MediaAttachment {
            url: "https://example.com/img.jpg".to_string(),
            media_type: hydeclaw_types::MediaType::Image,
            file_name: None,
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att]);
        assert_eq!(text, "[User attached an image: https://example.com/img.jpg]");
    }

    #[test]
    fn enrich_with_attachments_multiple_joined_with_newline() {
        let mut text = "look at this".to_string();
        let att1 = hydeclaw_types::MediaAttachment {
            url: "https://example.com/img.jpg".to_string(),
            media_type: hydeclaw_types::MediaType::Image,
            file_name: None,
            mime_type: None,
            file_size: None,
        };
        let att2 = hydeclaw_types::MediaAttachment {
            url: "https://example.com/audio.ogg".to_string(),
            media_type: hydeclaw_types::MediaType::Audio,
            file_name: None,
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att1, att2]);
        assert_eq!(
            text,
            "look at this\n[User attached an image: https://example.com/img.jpg]\n[User sent a voice message: https://example.com/audio.ogg]"
        );
    }

    #[test]
    fn enrich_with_attachments_document_with_filename() {
        let mut text = String::new();
        let att = hydeclaw_types::MediaAttachment {
            url: "https://example.com/doc.pdf".to_string(),
            media_type: hydeclaw_types::MediaType::Document,
            file_name: Some("report.pdf".to_string()),
            mime_type: None,
            file_size: None,
        };
        enrich_with_attachments(&mut text, &[att]);
        assert_eq!(text, "[User attached a document \"report.pdf\": https://example.com/doc.pdf]");
    }
}
