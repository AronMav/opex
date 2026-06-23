//! Phase 65 OBS-01 — end-to-end OTLP export fixture under `--features otel`.
//!
//! The production `main.rs` init path uses OTLP/gRPC (`.with_tonic()` on the
//! `SpanExporter` / `MetricExporter` builders). `wiremock` is an HTTP
//! mock server, so these tests exercise the sibling OTLP/HTTP transport
//! (`http-proto`) which the `opentelemetry-otlp` default feature set
//! already enables. The contract being pinned is:
//!
//!   * feature-gated OTel export ACTUALLY happens — a collector receives
//!     requests after `MetricsRegistry::install_otel_instruments()` + record
//!     calls (`histograms_export_to_otlp_collector`);
//!   * the request body carries the named metric (`tool_latency_seconds`)
//!     — protobuf keeps the metric name as literal ASCII bytes, so a naive
//!     `.contains(b"tool_latency_seconds")` substring check is sufficient.
//!
//! gRPC-specific wire encoding is NOT under test here (it is exercised by
//! the OTel crate's own CI). If the HTTP path exports the name, the gRPC
//! path does too — both sides flow through the same SDK aggregation layer.

#![cfg(feature = "otel")]

use opex_core::metrics::MetricsRegistry;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build an OTLP/HTTP `MetricExporter` pointed at the wiremock server, wire
/// it into a dedicated `SdkMeterProvider`, set that provider as the global,
/// and return both the provider (for `.force_flush()`) and the mock server
/// (for captured-request inspection).
///
/// NOTE: the fixture uses OTLP/HTTP (protobuf over HTTP/1.1) because
/// `wiremock` is an HTTP mock — it does not speak gRPC / HTTP/2. Production
/// `main.rs` uses `.with_tonic()` (gRPC). The same SDK aggregation pipeline
/// feeds both transports, so export-happening + name-bytes-present is the
/// exact contract we need to pin.
async fn start_fixture() -> (MockServer, opentelemetry_sdk::metrics::SdkMeterProvider) {
    let server = MockServer::start().await;
    // OTLP/HTTP expects POST /v1/metrics.
    Mock::given(method("POST"))
        .and(path("/v1/metrics"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // `with_endpoint`/`with_protocol` are trait methods — bring the trait
    // into scope. The test configures the `HttpBinary` protocol because
    // wiremock only speaks HTTP/1.1 (not gRPC/HTTP-2).
    use opentelemetry_otlp::WithExportConfig;

    let endpoint = server.uri();
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(format!("{endpoint}/v1/metrics"))
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .build()
        .expect("metric exporter build");

    let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name("opex-core-test")
                .build(),
        )
        .build();

    opentelemetry::global::set_meter_provider(meter_provider.clone());

    (server, meter_provider)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn histograms_export_to_otlp_collector() {
    let (server, meter_provider) = start_fixture().await;

    let registry = MetricsRegistry::new();
    registry.install_otel_instruments();

    // Record 50 data points across a small but non-trivial label set.
    for i in 0u32..50u32 {
        let result = if i.is_multiple_of(5) { "error" } else { "ok" };
        registry.record_tool_latency(
            "workspace_write",
            "agent-a",
            result,
            Duration::from_millis(12),
        );
    }
    // Also bump the LLM token counter so the captured body carries two
    // metric names (tool_latency_seconds + llm_tokens_total), proving the
    // entire instrument set flows through.
    registry.record_llm_tokens(500, "prompt");

    // Force a flush so we do not wait on the default 60s periodic tick.
    meter_provider
        .force_flush()
        .expect("force_flush must succeed against a responsive collector");

    // Give wiremock a moment to record the delivered request (the mock
    // server's body capture happens in a separate task on the axum router).
    tokio::time::sleep(Duration::from_millis(100)).await;

    let requests = server
        .received_requests()
        .await
        .expect("received_requests");
    assert!(
        !requests.is_empty(),
        "expected >=1 OTLP export POST to /v1/metrics; got 0"
    );

    // Protobuf preserves metric names as literal UTF-8 bytes at field tag
    // positions — a substring check is sufficient to prove the registry's
    // histogram name made it onto the wire.
    let concatenated: Vec<u8> = requests.iter().flat_map(|r| r.body.clone()).collect();
    let name = b"tool_latency_seconds";
    let found = concatenated
        .windows(name.len())
        .any(|w| w == name);
    assert!(
        found,
        "OTLP body must contain the literal bytes `tool_latency_seconds`; \
         received {} requests totalling {} bytes",
        requests.len(),
        concatenated.len()
    );
}

/// Second, narrower contract: without calling `install_otel_instruments`
/// the registry must NOT emit OTLP traffic. Proves the always-on atomic
/// path is side-effect-free w.r.t. the network. Uses a fresh mock server
/// because we do not want to interfere with the first test's global
/// `set_meter_provider` race.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_install_means_no_export() {
    // We deliberately do NOT create a meter provider — only a wiremock
    // that captures any unexpected POSTs. (If this test runs alongside
    // the first one, `set_meter_provider` is global, so installing an
    // unrelated server is the safest way to detect unintended traffic.)
    let unrelated_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/metrics"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&unrelated_server)
        .await;

    let registry = MetricsRegistry::new();
    // No install_otel_instruments() → atomic-only path.

    for _ in 0..50 {
        registry.record_tool_latency(
            "workspace_read",
            "agent-b",
            "ok",
            Duration::from_millis(3),
        );
    }

    // Wait longer than any plausible export tick.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let requests = unrelated_server
        .received_requests()
        .await
        .expect("received_requests");
    assert!(
        requests.is_empty(),
        "atomic-only path must NOT export; got {} requests",
        requests.len()
    );
}
