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

/// Adapter that lets the OTel TextMapPropagator read W3C trace headers
/// out of an Axum request's HeaderMap on the incoming side.
#[cfg(feature = "otel")]
struct AxumHeaderExtractor<'a>(&'a axum::http::HeaderMap);

#[cfg(feature = "otel")]
impl<'a> opentelemetry::propagation::Extractor for AxumHeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
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

/// Axum middleware factory: extract a W3C trace context from the incoming
/// request headers, wrap the request in a `tracing::Span` whose parent is
/// the extracted context, and pass the request through. Any spans created
/// downstream (e.g. `pipeline.execute`) inherit the upstream trace_id, so
/// a single Jaeger trace spans the upstream caller → Core → Toolgate path.
///
/// Without this, an external client that already carries a `traceparent`
/// (e.g. a future agent-to-agent call originating in another HydeClaw
/// instance, or a synthetic load-test rig that wants its trace to follow
/// the request) would have its context dropped at the gateway boundary
/// and Core would start a fresh, unrelated trace.
///
/// With the `otel` feature: pulls the registered `TextMapPropagator`,
/// extracts the parent context from headers, opens an `http_request`
/// span with method + path attributes, binds the extracted context as
/// its parent, and runs the rest of the request inside that span.
///
/// Without the `otel` feature: passes the request through unchanged
/// (no span created — keeps default builds free of any OTel imports).
#[cfg(feature = "otel")]
pub async fn extract_trace_context_layer(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tracing::Instrument;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&AxumHeaderExtractor(req.headers()))
    });

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let span = tracing::info_span!(
        "http_request",
        otel.kind = "server",
        http.method = %method,
        http.target = %path,
    );
    span.set_parent(parent_cx);

    next.run(req).instrument(span).await
}

#[cfg(not(feature = "otel"))]
pub async fn extract_trace_context_layer(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    next.run(req).await
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
