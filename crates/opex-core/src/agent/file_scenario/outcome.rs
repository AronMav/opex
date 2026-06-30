//! Core-owned outcome contract for the File Scenario Engine (FSE).
//! These types are populated deterministically by core (never LLM-reported):
//! `status: Ok` only when core observed a non-error result; the failure
//! statuses come from the HTTP code / a core-enforced per-execution timeout.

use serde::{Deserialize, Serialize};

/// Deterministic per-file processing status. Populated by core only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioStatus {
    Ok,
    Failed,
    Unsupported,
    TooLarge,
    Timeout,
}

/// Core-owned outcome envelope for one inbound attachment scenario run.
/// The bare signed URL must NOT survive in `summary_text` on `ok`/`save` —
/// it lives only in `artifact_urls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioOutcome {
    pub status: ScenarioStatus,
    pub summary_text: String,
    pub artifact_urls: Vec<String>,
    pub reason: Option<String>,
    /// `true` ONLY when this outcome is the async-video acceptance ack
    /// (`summarize_video` successfully enqueued a durable `video_jobs` row).
    /// Drives the pipeline short-circuit: when set, the LLM agent loop is
    /// skipped and the ack is persisted directly as the assistant reply so the
    /// agent does not redundantly try to process the YouTube link itself.
    /// `#[serde(default)]` keeps wire/DB compatibility for older payloads.
    #[serde(default)]
    pub video_accepted: bool,
}

impl ScenarioOutcome {
    /// Successful tool/toolgate result. `summary_text` is surfaced to the LLM;
    /// produced artifacts (signed URLs) go in `artifact_urls`.
    pub fn ok(summary_text: String, artifact_urls: Vec<String>) -> Self {
        Self { status: ScenarioStatus::Ok, summary_text, artifact_urls, reason: None, video_accepted: false }
    }

    /// Async-video acceptance ack: a `video_jobs` row was enqueued and `summary_text`
    /// is the user-facing "video accepted, preparing summary" message. Marks
    /// `video_accepted = true` so the pipeline short-circuits the LLM loop.
    pub fn video_accepted(summary_text: String, artifact_urls: Vec<String>) -> Self {
        Self { status: ScenarioStatus::Ok, summary_text, artifact_urls, reason: None, video_accepted: true }
    }

    /// The rowless universal fallback: nothing processed, file persisted.
    /// Same shape as `ok` so downstream rendering treats it uniformly.
    pub fn save(summary_text: String, artifact_urls: Vec<String>) -> Self {
        Self { status: ScenarioStatus::Ok, summary_text, artifact_urls, reason: None, video_accepted: false }
    }

    pub fn failed(reason: String) -> Self {
        Self { status: ScenarioStatus::Failed, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some(reason), video_accepted: false }
    }

    /// Fail-closed backstop: an `executor=tool` action_ref not in the dispatch table.
    pub fn unsupported(reason: String) -> Self {
        Self { status: ScenarioStatus::Unsupported, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some(reason), video_accepted: false }
    }

    pub fn timeout() -> Self {
        Self { status: ScenarioStatus::Timeout, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some("per-execution timeout".to_string()), video_accepted: false }
    }

    #[allow(dead_code)] // Phase 6: used when HTTP 413 from toolgate is surfaced as a UI chip message
    pub fn too_large(reason: String) -> Self {
        Self { status: ScenarioStatus::TooLarge, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some(reason), video_accepted: false }
    }
}

/// Re-export the single source of truth from `fse::allowlist`.
/// `file_scenario::FSE_DEFAULT_ALLOWLIST` and `fse::allowlist::FSE_DEFAULT_ALLOWLIST`
/// resolve to the same constant — no duplicate literal.
pub use crate::agent::fse::allowlist::FSE_DEFAULT_ALLOWLIST;

/// Map a toolgate/HTTP status code to a [`ScenarioStatus`]. 2xx => `Ok`;
/// 413 (payload too large) => `TooLarge`; 504 (gateway timeout) => `Timeout`;
/// every other non-2xx => `Failed`. (The Rust-side per-execution timeout maps
/// to `Timeout` separately in the dispatcher's `Err(_)` arm.)
pub fn status_from_http(code: u16) -> ScenarioStatus {
    match code {
        200..=299 => ScenarioStatus::Ok,
        413 => ScenarioStatus::TooLarge,
        504 => ScenarioStatus::Timeout,
        _ => ScenarioStatus::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serde_renames_to_snake_case() {
        // Wire contract: the SSE/HTTP JSON uses snake_case names.
        assert_eq!(serde_json::to_string(&ScenarioStatus::Ok).unwrap(), "\"ok\"");
        assert_eq!(serde_json::to_string(&ScenarioStatus::Failed).unwrap(), "\"failed\"");
        assert_eq!(serde_json::to_string(&ScenarioStatus::Unsupported).unwrap(), "\"unsupported\"");
        assert_eq!(serde_json::to_string(&ScenarioStatus::TooLarge).unwrap(), "\"too_large\"");
        assert_eq!(serde_json::to_string(&ScenarioStatus::Timeout).unwrap(), "\"timeout\"");
    }

    #[test]
    fn status_deserialises_from_snake_case() {
        let s: ScenarioStatus = serde_json::from_str("\"too_large\"").unwrap();
        assert_eq!(s, ScenarioStatus::TooLarge);
    }

    #[test]
    fn ok_helper_keeps_url_only_in_artifacts() {
        let o = ScenarioOutcome::ok("transcript here".into(), vec!["https://x/u/1".into()]);
        assert_eq!(o.status, ScenarioStatus::Ok);
        assert_eq!(o.summary_text, "transcript here");
        assert_eq!(o.artifact_urls, vec!["https://x/u/1".to_string()]);
        assert!(o.reason.is_none(), "ok outcome carries no reason");
    }

    #[test]
    fn failed_helper_sets_reason_and_empty_artifacts() {
        let o = ScenarioOutcome::failed("HTTP 502".into());
        assert_eq!(o.status, ScenarioStatus::Failed);
        assert_eq!(o.reason.as_deref(), Some("HTTP 502"));
        assert!(o.artifact_urls.is_empty());
    }

    #[test]
    fn timeout_helper_has_timeout_status() {
        assert_eq!(ScenarioOutcome::timeout().status, ScenarioStatus::Timeout);
    }

    #[test]
    fn allowlist_contains_exactly_the_five_builtins() {
        assert_eq!(FSE_DEFAULT_ALLOWLIST, &["transcribe", "describe", "extract_document", "save", "summarize_video"]);
        assert!(FSE_DEFAULT_ALLOWLIST.contains(&"transcribe"));
        assert!(!FSE_DEFAULT_ALLOWLIST.contains(&"code_exec"), "code_exec must never be allowlisted");
    }

    #[test]
    fn status_from_http_2xx_is_ok() {
        assert_eq!(status_from_http(200), ScenarioStatus::Ok);
        assert_eq!(status_from_http(204), ScenarioStatus::Ok);
    }

    #[test]
    fn status_from_http_413_is_too_large() {
        // toolgate raises 413 on oversized download (documents.py / download_limited).
        assert_eq!(status_from_http(413), ScenarioStatus::TooLarge);
    }

    #[test]
    fn status_from_http_504_is_timeout() {
        // toolgate maps an upstream URL timeout to 504 (vision.py describe_url).
        assert_eq!(status_from_http(504), ScenarioStatus::Timeout);
    }

    #[test]
    fn status_from_http_other_non_2xx_is_failed() {
        assert_eq!(status_from_http(400), ScenarioStatus::Failed);
        assert_eq!(status_from_http(415), ScenarioStatus::Failed);
        assert_eq!(status_from_http(502), ScenarioStatus::Failed);
        assert_eq!(status_from_http(503), ScenarioStatus::Failed);
    }

    // ── Wire-contract tests (R9): toolgate 4-key JSON ↔ ScenarioOutcome ──────

    #[test]
    fn toolgate_ok_json_deserialises_into_outcome() {
        // The EXACT 4-key JSON a toolgate ResultBuilder.text(...) emits (Phase 2).
        // `video_accepted` is absent on the wire; serde default => false (R9).
        let wire = r#"{"status":"ok","summary_text":"transcript here","artifact_urls":["/api/uploads/1?sig=x&exp=9"],"reason":null}"#;
        let o: ScenarioOutcome = serde_json::from_str(wire).unwrap();
        assert_eq!(o.status, ScenarioStatus::Ok);
        assert_eq!(o.summary_text, "transcript here");
        assert_eq!(o.artifact_urls, vec!["/api/uploads/1?sig=x&exp=9".to_string()]);
        assert!(o.reason.is_none());
        assert!(!o.video_accepted, "absent video_accepted must default to false");
    }

    #[test]
    fn toolgate_failed_json_deserialises_into_outcome() {
        let wire = r#"{"status":"failed","summary_text":"","artifact_urls":[],"reason":"HTTP 502"}"#;
        let o: ScenarioOutcome = serde_json::from_str(wire).unwrap();
        assert_eq!(o.status, ScenarioStatus::Failed);
        assert_eq!(o.reason.as_deref(), Some("HTTP 502"));
        assert!(o.artifact_urls.is_empty());
        assert!(!o.video_accepted);
    }

    #[test]
    fn toolgate_unsupported_too_large_timeout_statuses_deserialise() {
        for (wire_status, expected) in [
            ("too_large", ScenarioStatus::TooLarge),
            ("unsupported", ScenarioStatus::Unsupported),
            ("timeout", ScenarioStatus::Timeout),
        ] {
            let wire = format!(
                r#"{{"status":"{}","summary_text":"","artifact_urls":[],"reason":"x"}}"#,
                wire_status
            );
            let o: ScenarioOutcome = serde_json::from_str(&wire).unwrap();
            assert_eq!(o.status, expected, "status {} must map", wire_status);
        }
    }

    #[test]
    fn outcome_reserialises_to_toolgate_compatible_shape() {
        // Re-serialising the Rust type keeps the 4 toolgate keys with the right
        // names/values, plus the benign 5th `video_accepted` key (R9). The
        // assertion checks the toolgate-consumed keys, not exact-key equality.
        let o = ScenarioOutcome::ok(
            "hi".into(),
            vec!["/api/uploads/2?sig=y&exp=9".into()],
        );
        let json = serde_json::to_value(&o).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["summary_text"], "hi");
        assert_eq!(json["artifact_urls"][0], "/api/uploads/2?sig=y&exp=9");
        assert!(json["reason"].is_null());
        // The Rust type intentionally emits a 5th key the Python side omits.
        assert_eq!(json["video_accepted"], false, "video_accepted always serialises (R9)");
    }
}
