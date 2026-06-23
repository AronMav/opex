//! W3C traceparent injection (no-op without `otel` feature).
//!
//! Currently a no-op pass-through; matches the API of
//! `opex-core::trace_propagation::inject_trace_context` so the OTel feature
//! flag can be added later without breaking the call site.

use reqwest::RequestBuilder;

/// Inject W3C traceparent header into outgoing request. No-op in this crate
/// until the `otel` feature is added.
pub fn inject_trace_context(req: RequestBuilder) -> RequestBuilder {
    req
}
