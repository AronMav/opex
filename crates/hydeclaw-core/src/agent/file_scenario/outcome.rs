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
}

impl ScenarioOutcome {
    /// Successful tool/toolgate result. `summary_text` is surfaced to the LLM;
    /// produced artifacts (signed URLs) go in `artifact_urls`.
    pub fn ok(summary_text: String, artifact_urls: Vec<String>) -> Self {
        Self { status: ScenarioStatus::Ok, summary_text, artifact_urls, reason: None }
    }

    /// The rowless universal fallback: nothing processed, file persisted.
    /// Same shape as `ok` so downstream rendering treats it uniformly.
    pub fn save(summary_text: String, artifact_urls: Vec<String>) -> Self {
        Self { status: ScenarioStatus::Ok, summary_text, artifact_urls, reason: None }
    }

    pub fn failed(reason: String) -> Self {
        Self { status: ScenarioStatus::Failed, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some(reason) }
    }

    /// Fail-closed backstop: an `executor=tool` action_ref not in the dispatch table.
    pub fn unsupported(reason: String) -> Self {
        Self { status: ScenarioStatus::Unsupported, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some(reason) }
    }

    pub fn timeout() -> Self {
        Self { status: ScenarioStatus::Timeout, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some("per-execution timeout".to_string()) }
    }

    pub fn too_large(reason: String) -> Self {
        Self { status: ScenarioStatus::TooLarge, summary_text: String::new(), artifact_urls: Vec::new(), reason: Some(reason) }
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
}
