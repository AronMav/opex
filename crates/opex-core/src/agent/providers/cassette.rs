//! Cassette format for LLM provider HTTP traffic recording/replay.
//!
//! A cassette is a JSON file holding an ordered list of `{request, response}`
//! interactions captured from real LLM provider calls. In record mode the
//! [`CassetteTransport`](super::cassette_transport::CassetteTransport) passes
//! traffic through to the network and appends each interaction; in replay
//! mode it serves the Nth recorded response to the Nth runtime request
//! (sequential matching — correctly models retries/polling where identical
//! requests get different responses).
//!
//! Format (version 1):
//! ```json
//! {
//!   "version": 1,
//!   "metadata": { "name": "openai/tool-call", "recorded_at": "2026-07-05T..." },
//!   "interactions": [{
//!     "request": { "method": "POST", "url": "...", "headers": {...}, "body": "..." },
//!     "response": { "status": 200, "headers": {...}, "body": "...",
//!                   "body_encoding": "text" | "base64" }
//!   }]
//! }
//! ```
//!
//! Streaming responses are stored as the full body string (text/event-stream
//! concatenated) — the provider SSE parser runs identically against the
//! replayed full body. Binary responses use `body_encoding: "base64"`.
//!
//! Cassettes live under `tests/cassettes/{provider}/{scenario}.json` and are
//! committed to the repo so CI replays without keys/network/flake.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Cassette schema version. Bump on incompatible format changes.
const CASSETTE_VERSION: u32 = 1;

/// A complete cassette: metadata + ordered interaction list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cassette {
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<CassetteMetadata>,
    pub interactions: Vec<HttpInteraction>,
}

/// Free-form metadata block (name, recorded_at, tags, …).
pub type CassetteMetadata = serde_json::Value;

/// One recorded HTTP exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpInteraction {
    pub request: RequestSnapshot,
    pub response: ResponseSnapshot,
}

/// Captured request (post-redaction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSnapshot {
    pub method: String,
    pub url: String,
    /// Lowercased header name → value. Sensitive headers redacted to
    /// `"[REDACTED]"` by [`super::redaction`].
    pub headers: BTreeMap<String, String>,
    /// Request body as a string (JSON bodies canonicalized for matching).
    pub body: String,
}

/// Captured response (post-redaction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSnapshot {
    pub status: u16,
    /// Lowercased header name → value.
    pub headers: BTreeMap<String, String>,
    /// Body text (for text content types) or base64-encoded bytes (binary).
    pub body: String,
    /// `"text"` (default) or `"base64"`.
    #[serde(default = "default_body_encoding")]
    pub body_encoding: BodyEncoding,
}

fn default_body_encoding() -> BodyEncoding {
    BodyEncoding::Text
}

/// How the response body is encoded in the cassette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyEncoding {
    Text,
    Base64,
}

impl Cassette {
    /// Build a new empty cassette with version + optional metadata.
    pub fn new(metadata: Option<CassetteMetadata>) -> Self {
        Self {
            version: CASSETTE_VERSION,
            metadata,
            interactions: Vec::new(),
        }
    }

    /// Pretty-serialize to JSON string (reviewable diffs).
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    /// Deserialize from JSON string. Rejects wrong version.
    pub fn from_json(s: &str) -> Result<Self> {
        let cassette: Cassette = serde_json::from_str(s)?;
        if cassette.version != CASSETTE_VERSION {
            anyhow::bail!(
                "cassette version mismatch: expected {}, got {}",
                CASSETTE_VERSION,
                cassette.version
            );
        }
        Ok(cassette)
    }

    /// Write to a file (pretty-printed).
    pub fn write_to_file(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_json()?)?;
        Ok(())
    }

    /// Read from a file.
    pub fn read_from_file(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_json(&s)
    }

    /// Append an interaction.
    pub fn append(&mut self, interaction: HttpInteraction) {
        self.interactions.push(interaction);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_minimal_cassette() {
        let cassette = Cassette::new(None);
        let json = cassette.to_json().unwrap();
        let back = Cassette::from_json(&json).unwrap();
        assert_eq!(back.version, 1);
        assert!(back.interactions.is_empty());
    }

    #[test]
    fn round_trip_with_interaction() {
        let mut cassette = Cassette::new(Some(serde_json::json!({"name":"test"})));
        cassette.append(HttpInteraction {
            request: RequestSnapshot {
                method: "POST".into(),
                url: "https://api.example.com/v1/chat".into(),
                headers: {
                    let mut h = BTreeMap::new();
                    h.insert("content-type".into(), "application/json".into());
                    h.insert("authorization".into(), "[REDACTED]".into());
                    h
                },
                body: r#"{"model":"gpt-4"}"#.into(),
            },
            response: ResponseSnapshot {
                status: 200,
                headers: {
                    let mut h = BTreeMap::new();
                    h.insert("content-type".into(), "text/event-stream".into());
                    h
                },
                body: "data: {\"choices\":[]}\n\n".into(),
                body_encoding: BodyEncoding::Text,
            },
        });
        let json = cassette.to_json().unwrap();
        let back = Cassette::from_json(&json).unwrap();
        assert_eq!(back.interactions.len(), 1);
        assert_eq!(back.interactions[0].request.method, "POST");
        assert_eq!(
            back.interactions[0].request.headers.get("authorization").unwrap(),
            "[REDACTED]"
        );
        assert_eq!(back.interactions[0].response.status, 200);
        assert_eq!(back.interactions[0].response.body_encoding, BodyEncoding::Text);
    }

    #[test]
    fn rejects_wrong_version() {
        let json = r#"{"version":99,"interactions":[]}"#;
        assert!(Cassette::from_json(json).is_err());
    }

    #[test]
    fn body_encoding_defaults_to_text_when_absent() {
        let json = r#"{"version":1,"interactions":[{"request":{"method":"POST","url":"u","headers":{},"body":""},"response":{"status":200,"headers":{},"body":""}}]}"#;
        let cassette = Cassette::from_json(json).unwrap();
        assert_eq!(
            cassette.interactions[0].response.body_encoding,
            BodyEncoding::Text
        );
    }

    #[test]
    fn base64_body_encoding_round_trips() {
        let mut cassette = Cassette::new(None);
        cassette.append(HttpInteraction {
            request: RequestSnapshot {
                method: "POST".into(),
                url: "u".into(),
                headers: BTreeMap::new(),
                body: "".into(),
            },
            response: ResponseSnapshot {
                status: 200,
                headers: BTreeMap::new(),
                body: "AAAB".into(),
                body_encoding: BodyEncoding::Base64,
            },
        });
        let json = cassette.to_json().unwrap();
        assert!(json.contains(r#""body_encoding": "base64""#));
        let back = Cassette::from_json(&json).unwrap();
        assert_eq!(
            back.interactions[0].response.body_encoding,
            BodyEncoding::Base64
        );
    }
}