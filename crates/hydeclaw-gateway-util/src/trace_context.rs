//! W3C Trace Context middleware — Phase 65 OBS-04.
//!
//! Parse inbound `traceparent` header (version 00); on miss or on any
//! validation failure, generate a fresh UUID v4 trace_id (32 lowercase hex
//! characters). Attach the result as an Axum request extension + record it
//! on the current tracing span so every `tracing::info!/warn!/error!` call
//! emitted during the request lifecycle carries it.
//!
//! Purpose (ROADMAP Phase 65 success criterion #4):
//! `journalctl -u hydeclaw-core | grep <trace_id>` returns the full
//! lifecycle of one request.
//!
//! Placement: this middleware sits UPSTREAM of `auth_middleware` in the
//! router layer chain, so even 401/403/429 responses carry a trace_id for
//! diagnostic correlation.
//!
//! Scope per OBS-04: Rust-only in v0.19.  Toolgate (Python) + channels
//! (TypeScript) propagation is deferred to FUT-02 — they currently do not
//! forward or read `traceparent`.
//!
//! ── Leaf module discipline ──────────────────────────────────────────────
//! Deps: `std` + `axum` + `tracing` + `uuid` — NO `crate::*` imports.
//! This lets `lib.rs` re-export the module at path
//! `hydeclaw_core::gateway::trace_context` for integration tests without
//! cascading the rest of the gateway handler subtree.  See the 10-top-level-
//! module cap note at the top of `src/lib.rs`.

use axum::{extract::Request, middleware::Next, response::Response};
use tracing::Span;
use uuid::Uuid;

/// A parsed or freshly-generated W3C trace_id — always exactly 32 characters
/// of lowercase hexadecimal, never the reserved all-zero value.
///
/// Carried through a request via an Axum request extension
/// (`req.extensions().get::<TraceId>()`).  `Clone` is cheap — the inner
/// `String` is a 32-byte allocation.
#[derive(Debug, Clone)]
pub struct TraceId(pub String);

// ── W3C traceparent format constants ───────────────────────────────────────
// Full grammar for version 00:
//     traceparent = "00" "-" trace-id "-" parent-id "-" trace-flags
//     trace-id    = 32HEXDIGLC       (lowercase only per spec)
//     parent-id   = 16HEXDIGLC
//     trace-flags = 2HEXDIGLC
// Total bytes = 2 + 1 + 32 + 1 + 16 + 1 + 2 = 55.

const TRACEPARENT_LEN: usize = 55;
const VERSION_00_PREFIX: &[u8; 3] = b"00-";
const TRACE_ID_START: usize = 3;
const TRACE_ID_END: usize = 35; // exclusive
const FIRST_DASH_AFTER_TRACE_ID: usize = 35;
const SECOND_DASH: usize = 52;
const RESERVED_ZERO_TRACE_ID: &str = "00000000000000000000000000000000";

/// Parse the `traceparent` header and return the `trace-id` on success.
///
/// Returns `None` (fail-open → caller generates a fresh id) on any of:
///   * empty / wrong total length,
///   * version byte != `"00"`,
///   * dash characters in the wrong positions,
///   * non-hex characters in the trace-id segment,
///   * any uppercase hex character (W3C mandates lowercase),
///   * the reserved all-zero trace-id.
///
/// The parent-id and trace-flags segments are NOT validated here — we only
/// need the trace-id for correlation, and fail-open semantics mean a
/// malformed suffix must also fall through to generation (so downstream
/// systems never see a partial trace-id propagated alongside an invalid
/// parent-id).
pub fn parse_traceparent(header_value: &str) -> Option<TraceId> {
    if header_value.len() != TRACEPARENT_LEN {
        return None;
    }
    let bytes = header_value.as_bytes();

    // Strict version 00 per W3C — other versions MAY exist in the future;
    // fail-open so a new-version header from an upstream proxy does not
    // corrupt correlation IDs.
    if &bytes[..3] != VERSION_00_PREFIX {
        return None;
    }

    if bytes[FIRST_DASH_AFTER_TRACE_ID] != b'-' || bytes[SECOND_DASH] != b'-' {
        return None;
    }

    let trace_hex = &header_value[TRACE_ID_START..TRACE_ID_END];

    // Lowercase hex check — uppercase A–F is NOT valid per W3C grammar.
    if !trace_hex
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }

    // Reserved all-zero trace-id is invalid per W3C §3.2.2.3.
    if trace_hex == RESERVED_ZERO_TRACE_ID {
        return None;
    }

    Some(TraceId(trace_hex.to_owned()))
}

/// Generate a fresh trace_id — UUID v4 rendered as 32 lowercase hex chars
/// (no separating hyphens, matching W3C trace-id format).
#[must_use]
pub fn new_trace_id() -> TraceId {
    // `Uuid::simple()` yields the 32-char hex form without hyphens, always
    // lowercase, which is exactly the W3C trace-id format.
    TraceId(Uuid::new_v4().simple().to_string())
}

/// Axum middleware — runs UPSTREAM of `auth_middleware` so even 401/403/429
/// responses carry a trace_id.  Flow:
///   1. Read `traceparent` header (if present).
///   2. Parse via `parse_traceparent`; on `None`, generate via `new_trace_id`.
///   3. Insert the `TraceId` as an Axum request extension so downstream
///      handlers can fetch it via `Extension<TraceId>`.
///   4. Record `trace_id` on the current tracing span so log records emitted
///      by later middleware / handlers include it.
///   5. Forward to `next.run(req)` inside an `info_span!` that carries the
///      trace_id as a span field for structured-log backends.
pub async fn trace_context_middleware(mut req: Request, next: Next) -> Response {
    let trace_id = req
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_traceparent)
        .unwrap_or_else(new_trace_id);

    // Record on the current span — no-op if no subscriber is attached.
    Span::current().record("trace_id", trace_id.0.as_str());

    // Attach as a typed request extension for downstream handlers.
    req.extensions_mut().insert(trace_id.clone());

    // Scope the downstream execution in a span tagged with the trace_id.
    // `info_span!` is zero-cost when tracing is disabled.
    let span = tracing::info_span!("http_request", trace_id = %trace_id.0);
    let _enter = span.enter();

    next.run(req).await
}

// ── Unit tests (inline) ─────────────────────────────────────────────────────
// Test-surface duplication with `integration_trace_context.rs` is intentional:
// the unit tests here exercise parse_traceparent / new_trace_id without any
// Axum harness, so a regression is localised to this file.

#[cfg(test)]
mod tests {
    use super::{new_trace_id, parse_traceparent};

    #[test]
    fn parse_valid_returns_trace_id() {
        let t = parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
            .expect("valid traceparent");
        assert_eq!(t.0, "4bf92f3577b34da6a3ce929d0e0e4736");
    }

    #[test]
    fn reject_unsupported_version() {
        assert!(
            parse_traceparent("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none()
        );
    }

    #[test]
    fn reject_all_zero_trace_id() {
        assert!(
            parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none()
        );
    }

    #[test]
    fn reject_uppercase_hex() {
        assert!(
            parse_traceparent("00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01").is_none()
        );
    }

    #[test]
    fn reject_wrong_length() {
        // 31 hex chars instead of 32 → total length 54, not 55.
        let short = format!("00-{}-00f067aa0ba902b7-01", "a".repeat(31));
        assert!(parse_traceparent(&short).is_none());
    }

    #[test]
    fn reject_empty() {
        assert!(parse_traceparent("").is_none());
    }

    #[test]
    fn new_id_length_and_hex() {
        let id = new_trace_id();
        assert_eq!(id.0.len(), 32);
        assert!(
            id.0.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }

    /// Two consecutive `new_trace_id()` calls must not collide (randomness
    /// sanity check — UUID v4 gives 122 bits of entropy, so a real collision
    /// in-process is astronomically unlikely).
    #[test]
    fn new_id_uniqueness_sanity() {
        let a = new_trace_id();
        let b = new_trace_id();
        assert_ne!(a.0, b.0);
    }
}
