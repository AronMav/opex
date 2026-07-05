//! Secret redaction for cassette recording.
//!
//! Applied before persisting any interaction to a cassette file. Defense in
//! depth: header allow/redact lists, JSON body field redaction, and a
//! secret-pattern scanner that refuses to write a cassette containing known
//! credential shapes (OpenAI `sk-`, Anthropic `sk-ant-`, Google `AIza`, AWS
//! `AKIA`, bearer tokens, GitHub PATs, PEM private keys).
//!
//! Modeled on opencode's `packages/http-recorder/src/redaction.ts` +
//! `redactor.ts`, adapted to Rust.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use base64::Engine;
use regex::Regex;
use serde_json::Value;

use super::cassette::{BodyEncoding, HttpInteraction, RequestSnapshot, ResponseSnapshot};

/// Sentinel value replacing redacted secrets in cassettes.
pub(crate) const REDACTED: &str = "[REDACTED]";

/// Default sensitive request/response header names (lowercased) → value
/// replaced with `REDACTED` (header is still emitted, value redacted).
const DEFAULT_REDACT_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "proxy-authorization",
    "set-cookie",
    "x-api-key",
    "x-amz-security-token",
    "x-goog-api-key",
];

/// Default sensitive URL query parameter names (lowercased) → value redacted.
const DEFAULT_REDACT_QUERY_PARAMS: &[&str] = &[
    "access_token",
    "api-key",
    "api_key",
    "apikey",
    "code",
    "key",
    "signature",
    "sig",
    "token",
];

/// Default allowed request header names (lowercased). Anything NOT in this
/// list is dropped from the cassette entirely (minimize surface). Sensitive
/// names within this list are still value-redacted via `DEFAULT_REDACT_HEADERS`.
const DEFAULT_ALLOW_REQUEST_HEADERS: &[&str] = &[
    "content-type",
    "accept",
    "openai-beta",
    "anthropic-version",
    "x-api-key",
    "x-goog-api-key",
    "authorization",
];

/// Default allowed response header names (lowercased).
const DEFAULT_ALLOW_RESPONSE_HEADERS: &[&str] = &["content-type"];

/// Default sensitive JSON field names (normalized: lowercase, non-alphanumerics
/// stripped) → value replaced with `REDACTED` recursively.
const DEFAULT_REDACT_JSON_FIELDS: &[&str] = &[
    "access_token",
    "api_key",
    "apikey",
    "client_secret",
    "password",
    "refresh_token",
    "secret",
    "token",
];

/// Redact a single HTTP interaction in place (request + response).
pub(crate) fn redact_interaction(interaction: &mut HttpInteraction) {
    interaction.request.url = redact_url(&interaction.request.url);
    interaction.request.headers = redact_request_headers(&interaction.request.headers);
    interaction.request.body = redact_body(&interaction.request.body);
    interaction.response.headers = redact_response_headers(&interaction.response.headers);
    interaction.response.body = redact_body(&interaction.response.body);
}

/// Redact sensitive query params + userinfo in a URL string.
pub(crate) fn redact_url(url: &str) -> String {
    // Fast path: no `?` and no `@` → nothing to do.
    if !url.contains('?') && !url.contains('@') {
        return url.to_string();
    }
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            // Redact userinfo (username/password in URL).
            if !parsed.username().is_empty() || parsed.password().is_some() {
                let _ = parsed.set_username("[REDACTED]");
                let _ = parsed.set_password(Some("[REDACTED]"));
            }
            // Redact sensitive query params. Rebuild the query string in-place
            // on the parsed URL (preserves scheme+host+port+path+userinfo).
            let pairs: Vec<(String, String)> = parsed
                .query_pairs()
                .map(|(k, v)| {
                    let key_lower = k.to_lowercase();
                    if DEFAULT_REDACT_QUERY_PARAMS.contains(&key_lower.as_str()) {
                        (k.to_string(), REDACTED.to_string())
                    } else {
                        (k.to_string(), v.to_string())
                    }
                })
                .collect();
            if !pairs.is_empty() {
                let mut qs = url::form_urlencoded::Serializer::new(String::new());
                for (k, v) in &pairs {
                    qs.append_pair(k, v);
                }
                parsed.set_query(Some(&qs.finish()));
            }
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

/// Redact request headers: keep only allow-listed names; redact sensitive values.
pub(crate) fn redact_request_headers(
    headers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in headers {
        let lower = k.to_lowercase();
        if !DEFAULT_ALLOW_REQUEST_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        let val = if DEFAULT_REDACT_HEADERS.contains(&lower.as_str()) {
            REDACTED.to_string()
        } else {
            v.clone()
        };
        out.insert(lower, val);
    }
    out
}

/// Redact response headers: keep only allow-listed names; redact sensitive values.
pub(crate) fn redact_response_headers(
    headers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in headers {
        let lower = k.to_lowercase();
        if !DEFAULT_ALLOW_RESPONSE_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        let val = if DEFAULT_REDACT_HEADERS.contains(&lower.as_str()) {
            REDACTED.to_string()
        } else {
            v.clone()
        };
        out.insert(lower, val);
    }
    out
}

/// Redact sensitive JSON fields in a body string. Handles three shapes:
/// 1. A single JSON value → walk recursively, replace sensitive fields.
/// 2. An SSE stream (`text/event-stream`) → split on `\n\n`, strip the `data: `
///    prefix from each event, redact the JSON payload, reassemble.
/// 3. Anything else → return unchanged (binary bodies are base64-encoded).
pub(crate) fn redact_body(body: &str) -> String {
    // Fast path: valid single JSON value.
    if let Ok(mut value) = serde_json::from_str::<Value>(body) {
        redact_json_fields(&mut value);
        // Never fall back to the un-redacted original (L1: safe fallback).
        return serde_json::to_string(&value).unwrap_or_else(|_| "[REDACTED]".to_string());
    }
    // SSE stream: a sequence of `data: {…}\n\n` events. Redact each event's
    // JSON payload so secrets embedded in SSE responses don't leak.
    if body.contains("data: ") || body.contains("data:") {
        let mut out = String::with_capacity(body.len());
        for event in body.split("\n\n") {
            if event.is_empty() {
                continue;
            }
            for line in event.lines() {
                if let Some(json_str) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
                {
                    let redacted = redact_body(json_str.trim());
                    out.push_str("data: ");
                    out.push_str(&redacted);
                    out.push('\n');
                } else {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        return out;
    }
    body.to_string()
}

fn redact_json_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if is_sensitive_json_field(k) {
                    *v = Value::String(REDACTED.to_string());
                } else {
                    redact_json_fields(v);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json_fields(item);
            }
        }
        _ => {}
    }
}

fn is_sensitive_json_field(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    DEFAULT_REDACT_JSON_FIELDS
        .iter()
        .any(|field| normalize_field_name(field) == normalized)
}

fn normalize_field_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

// ── Secret-pattern scanner (defense in depth) ────────────────────────────────

/// Patterns that, if found anywhere in the interaction, indicate the cassette
/// would leak a real credential. The recorder refuses to write such a cassette
/// and fails the request. Compiled once (M4: avoid recompiling on every scan).
static SECRET_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

fn secret_patterns() -> &'static [Regex] {
    SECRET_PATTERNS.get_or_init(|| {
        vec![
            // Bearer tokens. The regex crate does not support look-ahead, so the
            // `[REDACTED]` exclusion is enforced in `scan_for_secrets` after match.
            Regex::new(r"(?i)bearer\s+[A-Za-z0-9\-._~+/]+={0,2}").unwrap(),
            // OpenAI keys
            Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap(),
            // Anthropic keys
            Regex::new(r"sk-ant-[A-Za-z0-9\-_]{20,}").unwrap(),
            // Google API keys
            Regex::new(r"AIza[0-9A-Za-z\-_]{35}").unwrap(),
            // AWS access key IDs
            Regex::new(r"\b(AKIA|ASIA)[0-9A-Z]{16}\b").unwrap(),
            // GitHub PATs
            Regex::new(r"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b").unwrap(),
            // PEM private keys
            Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap(),
        ]
    })
}

/// Scan an interaction for known secret patterns. Returns a description of the
/// first finding (for the error message), or `None` if clean.
///
/// Checks: request URL, request headers (values), request body, response
/// headers (values), response body. Excludes `Bearer [REDACTED]` matches
/// (regex crate lacks look-ahead — enforced here after match by checking the
/// token portion following the `bearer ` prefix).
// reviewed: floor_char_boundary-bounded slice — char boundary safe
#[allow(clippy::string_slice)]
pub(crate) fn scan_for_secrets(interaction: &HttpInteraction) -> Option<String> {
    let patterns = secret_patterns();
    let bearer_re = &patterns[0];
    let check = |s: &str| -> Option<String> {
        for re in patterns {
            if let Some(m) = re.find(s) {
                // Allow `Bearer [REDACTED]` (the bearer pattern is index 0).
                // The regex matches `bearer\s+<token>`; we strip the
                // `bearer\s+` prefix and compare the remaining token to the
                // `[REDACTED]` sentinel. If the token is the sentinel, this is
                // an already-redacted value, not a real secret — skip.
                if std::ptr::eq(re, bearer_re) {
                    let matched = m.as_str();
                    if let Some(token_part) = matched.split_whitespace().nth(1)
                        && token_part == REDACTED
                    {
                        continue;
                    }
                }
                return Some(format!("secret pattern `{}` matched", m.as_str()));
            }
        }
        None
    };
    if let Some(msg) = check(&interaction.request.url) {
        return Some(msg);
    }
    for v in interaction.request.headers.values() {
        if let Some(msg) = check(v) {
            return Some(msg);
        }
    }
    if let Some(msg) = check(&interaction.request.body) {
        return Some(msg);
    }
    for v in interaction.response.headers.values() {
        if let Some(msg) = check(v) {
            return Some(msg);
        }
    }
    if let Some(msg) = check(&interaction.response.body) {
        return Some(msg);
    }
    None
}

/// Encode raw response bytes into a `ResponseSnapshot` body + encoding.
pub(crate) fn encode_body(bytes: &[u8], content_type: &str) -> (String, BodyEncoding) {
    if is_text_content_type(content_type) {
        (
            String::from_utf8_lossy(bytes).into_owned(),
            BodyEncoding::Text,
        )
    } else {
        (
            base64::engine::general_purpose::STANDARD.encode(bytes),
            BodyEncoding::Base64,
        )
    }
}

/// Decode a `ResponseSnapshot` body back into raw bytes.
pub(crate) fn decode_body(body: &str, encoding: BodyEncoding) -> Vec<u8> {
    match encoding {
        BodyEncoding::Text => body.as_bytes().to_vec(),
        BodyEncoding::Base64 => {
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .unwrap_or_default()
        }
    }
}

fn is_text_content_type(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if media_type.is_empty() {
        return false;
    }
    if media_type.starts_with("text/") {
        return true;
    }
    const TEXT_TYPES: &[&str] = &[
        "application/json",
        "application/javascript",
        "application/xml",
        "application/graphql",
        "application/x-www-form-urlencoded",
        "application/sql",
        "application/yaml",
        "image/svg+xml",
    ];
    if media_type.ends_with("+json") || media_type.ends_with("+xml") {
        return true;
    }
    TEXT_TYPES.contains(&media_type.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_snapshot() -> RequestSnapshot {
        RequestSnapshot {
            method: "POST".into(),
            url: "https://api.example.com/v1/chat".into(),
            headers: BTreeMap::new(),
            body: "".into(),
        }
    }

    fn resp_snapshot() -> ResponseSnapshot {
        ResponseSnapshot {
            status: 200,
            headers: BTreeMap::new(),
            body: "".into(),
            body_encoding: BodyEncoding::Text,
        }
    }

    #[test]
    fn redact_url_redacts_key_query_param() {
        let url = "https://api.example.com/v1/models?key=AIzaSECRET&foo=bar";
        let redacted = redact_url(url);
        // [REDACTED] is percent-encoded as %5BREDACTED%5D in query strings.
        assert!(
            redacted.contains("key=%5BREDACTED%5D") || redacted.contains("key=[REDACTED]"),
            "redacted url: {redacted}"
        );
        assert!(redacted.contains("foo=bar"));
    }

    #[test]
    fn redact_url_preserves_port() {
        // L3 regression: port must not be dropped during query param redaction.
        let url = "https://api.example.com:8443/v1/models?key=secret";
        let redacted = redact_url(url);
        assert!(redacted.contains(":8443"), "port must be preserved: {redacted}");
    }

    #[test]
    fn redact_url_preserves_userinfo_with_query() {
        // L2 regression: userinfo redaction must survive when query params are present.
        let url = "https://user:pass@api.example.com:8443/v1?key=x";
        let redacted = redact_url(url);
        // [REDACTED] is percent-encoded in URLs (%5BREDACTED%5D).
        assert!(
            redacted.contains("[REDACTED]") || redacted.contains("%5BREDACTED%5D"),
            "userinfo must be redacted: {redacted}"
        );
        assert!(redacted.contains(":8443"), "port must be preserved: {redacted}");
    }

    #[test]
    fn redact_url_redacts_access_token() {
        let url = "https://example.com?access_token=mytoken&other=x";
        let redacted = redact_url(url);
        assert!(
            redacted.contains("%5BREDACTED%5D") || redacted.contains("[REDACTED]"),
            "redacted url: {redacted}"
        );
    }

    #[test]
    fn redact_request_headers_keeps_allow_list_redacts_sensitive() {
        let mut h = BTreeMap::new();
        h.insert("content-type".into(), "application/json".into());
        h.insert("authorization".into(), "Bearer sk-secret".into());
        h.insert("x-trace-id".into(), "should-dropped".into());
        h.insert("x-api-key".into(), "secret".into());
        let out = redact_request_headers(&h);
        assert_eq!(out.get("content-type").unwrap(), "application/json");
        assert_eq!(out.get("authorization").unwrap(), REDACTED);
        assert_eq!(out.get("x-api-key").unwrap(), REDACTED);
        assert!(!out.contains_key("x-trace-id"));
    }

    #[test]
    fn redact_body_replaces_token_field() {
        let body = r#"{"model":"gpt-4","token":"sk-secret","nested":{"api_key":"x"}}"#;
        let redacted = redact_body(body);
        assert!(redacted.contains("\"token\":\"[REDACTED]\""));
        assert!(redacted.contains("\"api_key\":\"[REDACTED]\""));
        assert!(redacted.contains("\"model\":\"gpt-4\""));
    }

    #[test]
    fn redact_body_handles_camel_case_field_names() {
        let body = r#"{"accessToken":"secret","RefreshToken":"x"}"#;
        let redacted = redact_body(body);
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn redact_body_non_json_unchanged() {
        let body = "plain text not json";
        assert_eq!(redact_body(body), "plain text not json");
    }

    #[test]
    fn redact_body_sse_stream_redacts_each_event() {
        // M1: SSE response bodies (text/event-stream) are sequences of
        // `data: {…}\n\n` events. redact_body must redact each event's JSON
        // payload, not skip the whole body.
        let body = "data: {\"token\":\"secret\",\"ok\":true}\n\ndata: {\"api_key\":\"key123\",\"result\":1}\n\n";
        let redacted = redact_body(body);
        assert!(redacted.contains("\"token\":\"[REDACTED]\""), "token redacted in event 1: {redacted}");
        assert!(redacted.contains("\"api_key\":\"[REDACTED]\""), "api_key redacted in event 2: {redacted}");
        assert!(redacted.contains("\"ok\":true"), "non-sensitive field preserved: {redacted}");
        assert!(redacted.contains("\"result\":1"), "non-sensitive field preserved: {redacted}");
    }

    #[test]
    fn scan_detects_openai_key_in_body() {
        let mut interaction = HttpInteraction {
            request: req_snapshot(),
            response: resp_snapshot(),
        };
        interaction.request.body = r#"{"api_key":"sk-12345678901234567890"}"#.into();
        assert!(scan_for_secrets(&interaction).is_some());
    }

    #[test]
    fn scan_detects_bearer_in_header() {
        let mut interaction = HttpInteraction {
            request: req_snapshot(),
            response: resp_snapshot(),
        };
        interaction
            .request
            .headers
            .insert("authorization".into(), "Bearer abc123def".into());
        assert!(scan_for_secrets(&interaction).is_some());
    }

    #[test]
    fn scan_detects_anthropic_key() {
        let mut interaction = HttpInteraction {
            request: req_snapshot(),
            response: resp_snapshot(),
        };
        interaction.request.body =
            "sk-ant-abc123def456ghi789jkl012mno345".into();
        assert!(scan_for_secrets(&interaction).is_some());
    }

    #[test]
    fn scan_allows_redacted_bearer() {
        let mut interaction = HttpInteraction {
            request: req_snapshot(),
            response: resp_snapshot(),
        };
        interaction
            .request
            .body
            .push_str(r#"{"authorization":"Bearer [REDACTED]"}"#);
        assert!(scan_for_secrets(&interaction).is_none());
    }

    #[test]
    fn scan_flags_bearer_followed_by_redacted_sentinel() {
        // Regression for H1: `Bearer abc123[REDACTED]` must be flagged — the
        // real secret `abc123` is immediately followed by the sentinel. The old
        // buggy logic skipped this; the new logic strips the `bearer ` prefix
        // and compares only the token portion to `[REDACTED]`.
        let mut interaction = HttpInteraction {
            request: req_snapshot(),
            response: resp_snapshot(),
        };
        interaction
            .request
            .body
            .push_str(r#"{"auth":"Bearer abc123[REDACTED]"}"#);
        assert!(scan_for_secrets(&interaction).is_some());
    }

    #[test]
    fn scan_flags_bearer_with_real_token() {
        let mut interaction = HttpInteraction {
            request: req_snapshot(),
            response: resp_snapshot(),
        };
        interaction
            .request
            .body
            .push_str(r#"{"auth":"Bearer sk-real-secret-token-12345"}"#);
        assert!(scan_for_secrets(&interaction).is_some());
    }

    #[test]
    fn scan_clean_interaction_returns_none() {
        let interaction = HttpInteraction {
            request: req_snapshot(),
            response: resp_snapshot(),
        };
        assert!(scan_for_secrets(&interaction).is_none());
    }

    #[test]
    fn encode_decode_text_round_trip() {
        let bytes = b"data: {}\n\n";
        let (body, enc) = encode_body(bytes, "text/event-stream");
        assert_eq!(enc, BodyEncoding::Text);
        assert_eq!(decode_body(&body, enc), bytes);
    }

    #[test]
    fn encode_decode_base64_round_trip() {
        let bytes: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 0xff, 0x00, 0x80];
        let (body, enc) = encode_body(&bytes, "image/png");
        assert_eq!(enc, BodyEncoding::Base64);
        assert_eq!(decode_body(&body, enc), bytes);
    }

    #[test]
    fn redact_interaction_redacts_all_fields() {
        let mut interaction = HttpInteraction {
            request: RequestSnapshot {
                method: "POST".into(),
                url: "https://api.example.com/v1/chat?key=secretkey".into(),
                headers: {
                    let mut h = BTreeMap::new();
                    h.insert("content-type".into(), "application/json".into());
                    h.insert("authorization".into(), "Bearer sk-secret".into());
                    h
                },
                body: r#"{"model":"gpt-4","token":"secret"}"#.into(),
            },
            response: ResponseSnapshot {
                status: 200,
                headers: {
                    let mut h = BTreeMap::new();
                    h.insert("content-type".into(), "text/event-stream".into());
                    h
                },
                body: "data: {}\n\n".into(),
                body_encoding: BodyEncoding::Text,
            },
        };
        redact_interaction(&mut interaction);
        assert!(
            interaction.request.url.contains("[REDACTED]")
                || interaction.request.url.contains("%5BREDACTED%5D"),
            "url not redacted: {}",
            interaction.request.url
        );
        assert_eq!(
            interaction.request.headers.get("authorization").unwrap(),
            REDACTED
        );
        assert!(interaction.request.body.contains("[REDACTED]"));
        assert_eq!(
            interaction
                .response
                .headers
                .get("content-type")
                .unwrap(),
            "text/event-stream"
        );
    }
}