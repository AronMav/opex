//! OpenTelemetry bootstrap for hydeclaw-memory-worker.
//!
//! Activated only when the `otel` feature is built AND the
//! `OTEL_EXPORTER_OTLP_ENDPOINT` env var is set. Otherwise falls back to
//! plain fmt-only tracing — matches the no-op contract of hydeclaw-core's
//! init.
//!
//! When active, the worker shows up in Jaeger as a separate service
//! (`hydeclaw-memory-worker` by default). It does NOT receive incoming
//! `traceparent` (worker is poll-driven, not request-driven), but its
//! outgoing calls to Toolgate `/v1/embeddings` get instrumented automatically
//! by the same `inject_trace_context` helper used by Core — copy or stub
//! locally; for now we expose only the SDK setup so internal `#[instrument]`
//! spans (e.g. on `process_task`) reach the collector.
//!
//! Why a separate service.name: Jaeger's service dropdown is the primary
//! grouping. Lumping the worker under "hydeclaw-core" would hide which
//! process generated a span — exactly the kind of confusion observability
//! is supposed to remove.

use opentelemetry_otlp::WithExportConfig;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn init() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "hydeclaw_memory_worker=info".into());
    let fmt_layer = tracing_subscriber::fmt::layer();

    let endpoint = match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(e) => e,
        Err(_) => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .init();
            return;
        }
    };

    let service = std::env::var("OTEL_SERVICE_NAME")
        .unwrap_or_else(|_| "hydeclaw-memory-worker".to_string());
    eprintln!("[otel] memory-worker exporting traces to {endpoint} as {service:?}");

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(service.clone())
        .build();

    let span_exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(e) => e,
        Err(err) => {
            eprintln!("[otel] memory-worker span exporter build failed: {err}; falling back");
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt_layer)
                .init();
            return;
        }
    };

    // ParentBased(TraceIdRatioBased(1.0)) — keep all traces by default,
    // operator can override via env if needed. Memory-worker volume is
    // low (a few embedding tasks per minute) so 100% sampling is fine.
    let sampler = opentelemetry_sdk::trace::Sampler::ParentBased(Box::new(
        opentelemetry_sdk::trace::Sampler::TraceIdRatioBased(1.0),
    ));
    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .with_resource(resource)
        .with_sampler(sampler)
        .build();
    opentelemetry::global::set_tracer_provider(tracer_provider.clone());
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let tracer = opentelemetry::trace::TracerProvider::tracer(&tracer_provider, "hydeclaw");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();
}
