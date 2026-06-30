//! Deterministic rewrite of the enriched user text from per-attachment outcomes.

use crate::agent::file_scenario::{ScenarioOutcome, ScenarioStatus};
use opex_types::{MediaAttachment, MediaType};

/// Build the bare hint string that `enrich_with_attachments` emitted for `att`,
/// so we can find-and-replace it deterministically (no fuzzy matching).
fn hint_for(att: &MediaAttachment) -> String {
    match att.media_type {
        MediaType::Image => format!("[User attached an image: {}]", att.url),
        MediaType::Audio => format!("[User sent a voice message: {}]", att.url),
        MediaType::Video => format!("[User sent a video: {}]", att.url),
        MediaType::Document => {
            let name = att.file_name.as_deref().unwrap_or("file");
            format!("[User attached a document \"{}\": {}]", name, att.url)
        }
    }
}

/// Rewrite the enriched text per the §4.4 outcome contract. `outcomes[i]`
/// corresponds to `attachments[i]`.
pub fn rewrite_enriched_text(
    text: &mut String,
    attachments: &[MediaAttachment],
    outcomes: &[ScenarioOutcome],
) {
    for (att, outcome) in attachments.iter().zip(outcomes.iter()) {
        let hint = hint_for(att);
        let kind = match att.media_type {
            MediaType::Image => "image",
            MediaType::Audio => "voice message",
            MediaType::Video => "video",
            MediaType::Document => "document",
        };
        match outcome.status {
            ScenarioStatus::Ok => {
                // image-ok keeps the original hint + appended <vision> (already in text).
                if att.media_type == MediaType::Image {
                    continue;
                }
                // Other ok/save: the bare URL must not survive in the prompt — strip the
                // hint entirely (the transcript/summary is already in `text`, the URL is
                // only in `artifact_urls`). If the hint is still present, drop it; trim
                // the orphaned newline left behind.
                strip_hint(text, &hint);
            }
            ScenarioStatus::Failed
            | ScenarioStatus::Unsupported
            | ScenarioStatus::TooLarge
            | ScenarioStatus::Timeout => {
                let reason = outcome.reason.as_deref().unwrap_or("unknown error");
                let replacement = format!(
                    "[{kind} received; automatic processing unavailable: {reason}; file saved — offer the user to retry or just keep it]"
                );
                if replace_hint(text, &hint, &replacement) {
                    // replaced in-place
                } else {
                    // Hint already consumed by the built-in; append the signal so the
                    // LLM still learns the file failed.
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&replacement);
                }
            }
        }
    }
}

/// Replace the `hint` bracket block in `text` with `replacement`. Returns `true`
/// if a replacement was made (either exact match or URL-anchored fallback).
fn replace_hint(text: &mut String, hint: &str, replacement: &str) -> bool {
    // Fast path: exact match.
    if text.contains(hint) {
        *text = text.replace(hint, replacement);
        return true;
    }

    // Fallback: locate the URL inside any `[…]` bracket block.
    if let Some(url_start) = hint.rfind(": ") {
        let url = &hint[url_start + 2..hint.len() - 1];
        if let Some(url_pos) = text.find(url) {
            let bracket_start = text[..url_pos].rfind('[').unwrap_or(url_pos);
            let bracket_end = text[url_pos..].find(']').map(|o| url_pos + o + 1).unwrap_or(url_pos + url.len());
            text.replace_range(bracket_start..bracket_end, replacement);
            return true;
        }
    }
    false
}

/// Remove `hint` from `text`, collapsing a dangling leading/trailing newline so
/// we don't leave a blank line where the hint was.
///
/// If the exact `hint` is not found (e.g. the document filename in the emitted
/// hint differs from what we reconstructed because `att.file_name` was `None`),
/// falls back to finding a `[…<url>…]` bracket block containing the attachment
/// URL — the URL is always the stable anchor.
fn strip_hint(text: &mut String, hint: &str) {
    // Fast path: exact match.
    if let Some(idx) = text.find(hint) {
        excise(text, idx, idx + hint.len());
        return;
    }

    // Fallback: locate the URL inside any `[…]` bracket block.
    // Extract the URL from the hint (it always ends the hint before `]`).
    if let Some(url_start) = hint.rfind(": ") {
        let url = &hint[url_start + 2..hint.len() - 1]; // strip ": " prefix and trailing `]`
        if let Some(url_pos) = text.find(url) {
            // Walk left to find the opening `[`.
            let bracket_start = text[..url_pos].rfind('[').unwrap_or(url_pos);
            // Walk right to find the closing `]`.
            let bracket_end = text[url_pos..].find(']').map(|o| url_pos + o + 1).unwrap_or(url_pos + url.len());
            excise(text, bracket_start, bracket_end);
        }
    }
}

/// Remove `text[start..end]`, also eating one preceding `\n` to avoid a blank line.
fn excise(text: &mut String, start: usize, end: usize) {
    let start = if start > 0 && text.as_bytes()[start - 1] == b'\n' { start - 1 } else { start };
    text.replace_range(start..end, "");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::file_scenario::{ScenarioOutcome, ScenarioStatus};
    use opex_types::{MediaAttachment, MediaType};

    fn att(url: &str, mt: MediaType) -> MediaAttachment {
        MediaAttachment { url: url.into(), media_type: mt, file_name: None, mime_type: None, file_size: None }
    }

    const SIGNED: &str = "https://h/api/uploads/abc?sig=deadbeef&exp=99";

    #[test]
    fn non_ok_image_leaves_no_signed_url() {
        let mut text = format!("[User attached an image: {SIGNED}]");
        let outcomes = vec![ScenarioOutcome {
            status: ScenarioStatus::Failed,
            summary_text: String::new(),
            artifact_urls: vec![],
            reason: Some("vision 503".into()),
            video_accepted: false,
            post_action: None,
        }];
        rewrite_enriched_text(&mut text, &[att(SIGNED, MediaType::Image)], &outcomes);
        assert!(!text.contains(SIGNED), "signed URL must be gone on failure: {text}");
        assert!(text.contains("automatic processing unavailable"), "explicit signal expected: {text}");
        assert!(text.contains("vision 503"), "reason surfaced: {text}");
    }

    #[test]
    fn ok_audio_strips_url() {
        // transcribe already replaced the hint; assert any leftover bare URL is stripped.
        let mut text = format!("[User sent a voice message: {SIGNED}]");
        let outcomes = vec![ScenarioOutcome {
            status: ScenarioStatus::Ok,
            summary_text: "hello".into(),
            artifact_urls: vec![],
            reason: None,
            video_accepted: false,
            post_action: None,
        }];
        rewrite_enriched_text(&mut text, &[att(SIGNED, MediaType::Audio)], &outcomes);
        assert!(!text.contains(SIGNED), "ok audio must not leave a bare signed URL: {text}");
    }

    #[test]
    fn ok_image_keeps_url() {
        let mut text = format!("[User attached an image: {SIGNED}]\n<vision>a cat</vision>");
        let outcomes = vec![ScenarioOutcome {
            status: ScenarioStatus::Ok,
            summary_text: "a cat".into(),
            artifact_urls: vec![],
            reason: None,
            video_accepted: false,
            post_action: None,
        }];
        rewrite_enriched_text(&mut text, &[att(SIGNED, MediaType::Image)], &outcomes);
        assert!(text.contains(SIGNED), "image-ok must keep the URL for FilePart reconstruction: {text}");
        assert!(text.contains("<vision>a cat</vision>"), "vision text preserved: {text}");
    }

    #[test]
    fn save_puts_url_only_in_artifacts() {
        let mut text = format!("[User attached a document \"r.pdf\": {SIGNED}]");
        let outcomes = vec![ScenarioOutcome {
            status: ScenarioStatus::Ok, // save success is reported as Ok
            summary_text: "saved".into(),
            artifact_urls: vec![SIGNED.into()],
            reason: None,
            video_accepted: false,
            post_action: None,
        }];
        rewrite_enriched_text(&mut text, &[att(SIGNED, MediaType::Document)], &outcomes);
        assert!(!text.contains(SIGNED), "non-image ok must not leave a bare URL in the prompt: {text}");
    }
}
