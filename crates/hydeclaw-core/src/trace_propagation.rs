//! W3C trace context propagation for outgoing HTTP calls.
//!
//! Without this, `pipeline.execute` spans on Core stay disconnected from
//! `POST /v1/embeddings` spans on Toolgate even when both processes export
//! to the same Jaeger collector — Jaeger can't link them because no
//! `traceparent` header carries the trace_id between processes.
//!
//! Use [`inject_trace_context`] to wrap a `reqwest::RequestBuilder`. With
//! the `otel` feature enabled it pulls the current `tracing::Span`'s
//! OpenTelemetry context and injects W3C `traceparent` + `tracestate`
//! headers via the global propagator. Without the feature it's a no-op
//! that returns the builder unchanged — keeps default builds free of
//! the OTel dependency tree.
//!
//! Why a free function instead of `reqwest-middleware`: the middleware
//! crate would force every call site to switch its `Client` type, which
//! would touch ~50 call sites across the codebase. A per-call wrapper
//! is opt-in: hot paths that talk to other instrumented services
//! (Toolgate, Channels) get propagation; everything else stays untouched.

use reqwest::RequestBuilder;

#[cfg(feature = "otel")]
use std::collections::HashMap;

#[cfg(feature = "otel")]
struct HeaderInjector<'a>(&'a mut HashMap<String, String>);

#[cfg(feature = "otel")]
impl<'a> opentelemetry::propagation::Injector for HeaderInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
}

/// Inject the current span's W3C trace context into an outgoing request.
///
/// With `otel` feature: pulls `tracing::Span::current()` → OTel context
/// via `OpenTelemetrySpanExt` and asks the global propagator to write
/// `traceparent` (and `tracestate` when present) headers.
///
/// Without `otel` feature: returns the builder unchanged.
#[cfg(feature = "otel")]
pub fn inject_trace_context(builder: RequestBuilder) -> RequestBuilder {
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let cx = tracing::Span::current().context();
    let mut headers: HashMap<String, String> = HashMap::new();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&cx, &mut HeaderInjector(&mut headers));
    });

    let mut builder = builder;
    for (k, v) in headers {
        builder = builder.header(k, v);
    }
    builder
}

#[cfg(not(feature = "otel"))]
pub fn inject_trace_context(builder: RequestBuilder) -> RequestBuilder {
    builder
}

#[cfg(test)]
#[cfg(feature = "otel")]
mod tests {
    use super::*;
    use reqwest::Client;

    /// `inject_trace_context` must be a no-op (idempotent) when there is
    /// no active OTel span context — the default propagator (TraceContext)
    /// writes nothing in that case so the request goes out unchanged.
    #[test]
    fn no_active_span_is_noop() {
        let client = Client::new();
        let builder = client.get("http://example.invalid/");
        let _wrapped = inject_trace_context(builder);
        // Compiles + doesn't panic — full propagation behavior is
        // pinned by the live cross-process trace check on Pi (see
        // observability-setup.md "Validating Spans Under Load").
    }
}
