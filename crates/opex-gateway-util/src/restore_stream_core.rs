//! Phase 64 SEC-04 — TYPE-GENERIC streaming body cap + struson walker primitives.
//!
//! This is a **leaf** module: zero `crate::*` imports. Safe to re-export via
//! `src/lib.rs` so integration tests (`tests/integration_backup_size_cap.rs`) can
//! exercise the SEC-04 enforcement path without cascading the binary's module tree.
//!
//! Two responsibilities:
//!
//!   * `check_content_length_cap(headers, cap_bytes)` — Content-Length fast-path
//!     (<1ms, zero body bytes read). Returns `Some((413, json_body))` on overage.
//!
//!   * `drain_body_with_cap(stream, cap_bytes)` — on-the-fly byte counter over an
//!     `impl Stream<Item = Result<Bytes, E>>`. Aborts with `Err(CapExceeded)` the
//!     moment cumulative bytes cross the cap. Covers missing/lying Content-Length.
//!
//!   * `parse_stream_typed::<T>(reader)` — struson-backed pull-parse that deserializes
//!     into ANY `T: DeserializeOwned`. Kept type-generic so this module stays free of
//!     any `crate::*` references. The handler-facing `BackupFile` walker lives in
//!     `gateway::restore_stream` and delegates field-by-field (section walk) because
//!     `BackupFile` has non-serde-default required fields we report discretely.
//!
//! NO `serde_json::from_slice(&buf)` fallback — CONTEXT D-SEC-04 forbids it.

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use struson::reader::{JsonReader, JsonStreamReader};

/// Failure while draining the request body under the size cap.
///
/// F115: a stream/transport error (client dropped, TCP reset, chunked-decode
/// failure) MUST be distinguishable from a genuine cap breach. Reporting an I/O
/// error as `CapExceeded { observed_bytes < cap_bytes }` produced a
/// self-contradictory 413 that sent operators down the wrong path (shrinking a
/// backup that was never too big) instead of investigating the transport.
#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    #[error("payload exceeds max_restore_size_mb ({observed_bytes} bytes > {cap_bytes} bytes)")]
    CapExceeded { observed_bytes: usize, cap_bytes: usize },
    #[error("restore body stream error: {0}")]
    Stream(String),
}

/// Content-Length header fast-path. Returns `Some((status, json_body))` when the
/// header parses AND exceeds the cap. Fast-path contract: <1ms, no body read.
pub fn check_content_length_cap(
    headers: &HeaderMap,
    cap_bytes: usize,
) -> Option<(StatusCode, Vec<u8>)> {
    let cl = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())?;
    if cl > cap_bytes {
        let body = json!({
            "error": "payload exceeds max_restore_size_mb",
            "cap_bytes": cap_bytes,
            "content_length": cl,
        });
        let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
        Some((StatusCode::PAYLOAD_TOO_LARGE, bytes))
    } else {
        None
    }
}

/// Streaming body drain with a hard byte cap. Aborts the moment cumulative bytes
/// cross `cap_bytes` regardless of whether the stream advertised Content-Length.
///
/// The item type is `Result<Bytes, E>` so this works for both axum's `BodyDataStream`
/// (`E = axum::Error`) and plain `std::io::Error` streams from the test harness.
pub async fn drain_body_with_cap<S, E>(mut stream: S, cap_bytes: usize) -> Result<Vec<u8>, DrainError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buf: Vec<u8> = Vec::with_capacity(1024 * 1024);
    while let Some(chunk) = stream.next().await {
        let c = match chunk {
            Ok(b) => b,
            Err(e) => {
                // F115: a transport/IO failure is NOT a cap breach — report it as
                // such so the handler maps it to 400/500 with the real cause,
                // instead of a misleading 413 with observed_bytes < cap_bytes.
                tracing::warn!(error = %e, "restore body stream error");
                return Err(DrainError::Stream(e.to_string()));
            }
        };
        if buf.len() + c.len() > cap_bytes {
            return Err(DrainError::CapExceeded {
                observed_bytes: buf.len() + c.len(),
                cap_bytes,
            });
        }
        buf.extend_from_slice(&c);
    }
    Ok(buf)
}

/// Sanity-check helper for integration tests: pull a top-level JSON Value via the
/// struson streaming reader (NOT serde_json::from_slice). Used to verify the
/// streaming path works end-to-end on a 100MB fixture without the test needing to
/// pull in the binary-crate `BackupFile` type.
///
/// Returns the parsed `serde_json::Value` or a `String` error. This is the
/// leaf-safe counterpart to the full `parse_backup_stream` in
/// `handlers::backup` (which is typed to `BackupFile`).
///
/// Only referenced from integration tests via the lib facade; the binary target
/// uses the typed `parse_backup_stream` walker.
pub fn parse_stream_value<R: std::io::Read>(reader: R) -> Result<Value, String> {
    let mut json = JsonStreamReader::new(reader);
    json.deserialize_next::<Value>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use futures_util::stream;

    fn cap_bytes() -> usize {
        500 * 1024 * 1024
    }

    #[test]
    fn content_length_cap_below_passes() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::CONTENT_LENGTH,
            HeaderValue::from_str("400").unwrap(),
        );
        assert!(check_content_length_cap(&h, cap_bytes()).is_none());
    }

    #[test]
    fn content_length_cap_over_rejects_413() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::CONTENT_LENGTH,
            HeaderValue::from_str(&(cap_bytes() + 1).to_string()).unwrap(),
        );
        let (status, body) = check_content_length_cap(&h, cap_bytes()).unwrap();
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "payload exceeds max_restore_size_mb");
    }

    #[test]
    fn content_length_cap_missing_header_passes_through() {
        let h = HeaderMap::new();
        assert!(check_content_length_cap(&h, cap_bytes()).is_none());
    }

    #[test]
    fn content_length_cap_exact_boundary_passes() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::CONTENT_LENGTH,
            HeaderValue::from_str(&cap_bytes().to_string()).unwrap(),
        );
        assert!(check_content_length_cap(&h, cap_bytes()).is_none());
    }

    #[tokio::test]
    async fn drain_body_under_cap_collects_all() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"hello ")),
            Ok(Bytes::from_static(b"world")),
        ];
        let s = stream::iter(chunks);
        let out = drain_body_with_cap(s, 1024).await.unwrap();
        assert_eq!(&out, b"hello world");
    }

    #[tokio::test]
    async fn drain_body_over_cap_aborts() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from(vec![0u8; 600])),
            Ok(Bytes::from(vec![0u8; 600])),
        ];
        let s = stream::iter(chunks);
        let err = drain_body_with_cap(s, 1000).await.unwrap_err();
        match err {
            DrainError::CapExceeded { observed_bytes, cap_bytes } => {
                assert!(observed_bytes > 1000);
                assert_eq!(cap_bytes, 1000);
            }
            other => panic!("expected CapExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drain_body_stream_error_is_not_cap_exceeded() {
        // F115: a mid-stream IO error must surface as Stream, not a bogus 413.
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"partial")),
            Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset")),
        ];
        let s = stream::iter(chunks);
        let err = drain_body_with_cap(s, 1_000_000).await.unwrap_err();
        assert!(matches!(err, DrainError::Stream(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn drain_body_exact_cap_passes() {
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            vec![Ok(Bytes::from(vec![0u8; 1000]))];
        let s = stream::iter(chunks);
        let out = drain_body_with_cap(s, 1000).await.unwrap();
        assert_eq!(out.len(), 1000);
    }
}
