//! URL extraction and content helpers — standalone functions used by the engine.
//!
//! Safety note: `auto_transcribe_audio` and `auto_describe_images` use `http_client`
//! (no SSRF filtering) because attachment downloads always go to Core's own
//! `/uploads/` endpoint via localhost. The caller must ensure attachments originate
//! from trusted internal sources.

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
    // Extract path from URL (e.g. "/uploads/uuid.jpg") or fall back to full URL
    let path = if let Some(idx) = att_url.find("/uploads/") {
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

/// Auto-transcribe audio attachments via toolgate STT before sending to LLM.
/// Downloads audio bytes from Core (localhost) then uploads to toolgate /transcribe
/// (file upload endpoint — no SSRF check, unlike /transcribe-url).
pub(crate) async fn auto_transcribe_audio(
    text: &mut String,
    attachments: &[hydeclaw_types::MediaAttachment],
    toolgate_url: &str,
    language: &str,
    http_client: &reqwest::Client,
    gateway_listen: &str,
) {
    use hydeclaw_types::MediaType;
    for att in attachments {
        if att.media_type != MediaType::Audio {
            continue;
        }
        // Step 1: Download audio bytes via localhost (avoids TLS issues with public_url)
        let local_url = uploads_local_url(&att.url, gateway_listen);
        let audio_bytes = match http_client.get(&local_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(b) => b,
                Err(e) => { tracing::warn!(error = %e, "failed to read audio bytes"); continue; }
            },
            Ok(resp) => { tracing::warn!(url = %local_url, status = %resp.status(), "failed to download audio"); continue; }
            Err(e) => { tracing::warn!(error = %e, url = %local_url, "failed to download audio from Core"); continue; }
        };

        // Step 2: Upload bytes to toolgate /transcribe (file upload, no SSRF check)
        let url = format!("{}/transcribe", toolgate_url.trim_end_matches('/'));
        let filename = att.url.split('/').next_back().unwrap_or("voice.ogg");
        let part = reqwest::multipart::Part::bytes(audio_bytes.to_vec())
            .file_name(filename.to_string())
            .mime_str("audio/ogg")
            .unwrap_or_else(|_| reqwest::multipart::Part::bytes(audio_bytes.to_vec()));
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("language", language.to_string());

        // Inject W3C traceparent header so the toolgate-side STT span
        // attaches to the current Core parent span. No-op without `otel`.
        let req = http_client.post(&url).multipart(form);
        let req = crate::trace_propagation::inject_trace_context(req);
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(data) = resp.json::<serde_json::Value>().await
                    && let Some(transcript) = data["text"].as_str()
                        && !transcript.is_empty() {
                            let url_hint = format!("[User sent a voice message: {}]", att.url);
                            let replacement = format!("[User's voice message (transcribed): {transcript}]");
                            *text = text.replace(&url_hint, &replacement);
                            tracing::info!(len = transcript.len(), "auto-transcribed voice message");
                        }
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(%status, body = %body, "auto-transcribe failed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "auto-transcribe request failed");
            }
        }
    }
}

/// Auto-describe image attachments via toolgate vision before sending to LLM.
/// Downloads image bytes from Core (localhost) then POSTs to toolgate /describe.
/// Replaces the `[User attached an image: ...]` hint with the vision description.
/// Silently skips if vision provider is inactive (503) or toolgate unreachable.
pub(crate) async fn auto_describe_images(
    text: &mut String,
    attachments: &[hydeclaw_types::MediaAttachment],
    toolgate_url: &str,
    language: &str,
    http_client: &reqwest::Client,
    gateway_listen: &str,
) {
    use hydeclaw_types::MediaType;
    for att in attachments {
        if att.media_type != MediaType::Image {
            continue;
        }
        // Step 1: Download image bytes via localhost (avoids TLS issues with public_url)
        let local_url = uploads_local_url(&att.url, gateway_listen);
        let image_bytes = match http_client.get(&local_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(b) => b,
                Err(e) => { tracing::warn!(error = %e, "failed to read image bytes"); continue; }
            },
            Ok(resp) => { tracing::warn!(url = %local_url, status = %resp.status(), "failed to download image"); continue; }
            Err(e) => { tracing::warn!(error = %e, url = %local_url, "failed to download image from Core"); continue; }
        };

        // Step 2: POST bytes to toolgate /describe (multipart file upload, no SSRF check)
        let url = format!("{}/describe", toolgate_url.trim_end_matches('/'));
        let filename = att.url.split('/').next_back().unwrap_or("image.jpg").to_string();
        let mime = att.mime_type.as_deref().unwrap_or("image/jpeg").to_string();
        let part = reqwest::multipart::Part::bytes(image_bytes.to_vec())
            .file_name(filename)
            .mime_str(&mime)
            .unwrap_or_else(|_| reqwest::multipart::Part::bytes(image_bytes.to_vec()));
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("language", language.to_string());

        // Inject W3C traceparent so the toolgate-side vision span
        // attaches to the current Core parent. No-op without `otel`.
        let req = http_client.post(&url).multipart(form);
        let req = crate::trace_propagation::inject_trace_context(req);
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(data) = resp.json::<serde_json::Value>().await
                    && let Some(description) = data["description"].as_str()
                    && !description.is_empty()
                {
                    let url_hint = format!("[User attached an image: {}]", att.url);
                    // Keep the original URL hint so the UI can reconstruct the image FilePart
                    // from history; append the vision description for the LLM context only.
                    // Use <vision>...</vision> tags — not markdown, won't confuse rendering.
                    let replacement = format!("[User attached an image: {}]\n<vision>{description}</vision>", att.url);
                    *text = text.replace(&url_hint, &replacement);
                    tracing::info!(len = description.len(), "auto-described image via vision");
                }
            }
            Ok(resp) if resp.status().as_u16() == 503 => {
                tracing::debug!(url = %att.url, "vision provider not active, keeping image hint");
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(%status, body = %body, "vision describe failed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "vision describe request failed");
            }
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
