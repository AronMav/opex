//! RES-02 + OBS-05: Validate `/api/health/dashboard` contract.
//!
//! Six tests cover:
//!   1. Registry empty → empty snapshot (baseline surface)
//!   2. Registry + BoundMetricsProbe → recorded drops readable via probe
//!   3. Handler-level grouping: `build_dashboard_body` (the pure function
//!      `api_health_dashboard` delegates to) produces `{agent: {event_type:
//!      count}}` nested JSON — NOT flat `"agent:event_type"` pair-keys.
//!   4. Phase 65 OBS-05: `build_dashboard_body_with_snapshot` emits ≥10
//!      named fields covering cluster-level counters (active_agents, SSE
//!      streams, approval waiters, rate-limiter sizes, DB pool, uptime, …).
//!   5. Phase 65 OBS-05 regression pin: the nested
//!      `sse_events_dropped_total` shape from Phase 62 is preserved
//!      byte-identically under the new snapshot-aware call path.
//!   6. Phase 65 OBS-05 regression pin: Phase 64 `csp_violations` +
//!      `csp_violations_overflow` remain untouched.

mod support;

use std::sync::Arc;
use std::time::Duration;

use opex_core::metrics::{
    build_dashboard_body, build_dashboard_body_with_snapshot, DashboardSnapshot, MetricsRegistry,
};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_empty_returns_empty_snapshot() {
    timeout(Duration::from_secs(10), async {
        let registry = Arc::new(MetricsRegistry::new());
        let snapshot = registry.snapshot_sse_drops();
        assert!(snapshot.is_empty(), "fresh registry must be empty");
    })
    .await
    .expect("test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registry_reflects_recorded_drops_via_probe() {
    timeout(Duration::from_secs(10), async {
        let registry = Arc::new(MetricsRegistry::new());
        registry.record_sse_drop("agent-a", "text-delta");
        registry.record_sse_drop("agent-a", "text-delta");
        registry.record_sse_drop("agent-b", "finish");

        let snapshot = registry.snapshot_sse_drops();
        assert_eq!(
            snapshot.get(&("agent-a".to_string(), "text-delta".to_string())),
            Some(&2)
        );
        assert_eq!(
            snapshot.get(&("agent-b".to_string(), "finish".to_string())),
            Some(&1)
        );

        // BoundMetricsProbe reads via the same path the handler uses.
        let probe = support::MetricsProbe::new().connect(registry.clone());
        assert_eq!(probe.read_counter("agent-a", "text-delta"), 2);
        assert_eq!(probe.read_counter("agent-b", "finish"), 1);
        assert_eq!(probe.read_counter("agent-missing", "text-delta"), 0);
    })
    .await
    .expect("test timed out");
}

/// Handler-shape test. Exercises the flat→nested transformation that the
/// `/api/health/dashboard` handler `api_health_dashboard(` delegates to via
/// `build_dashboard_body`. Asserts nested grouping, stable keys, and
/// lossless serde round-trip. Explicitly rejects a flat `"agent:event_type"`
/// pair-key representation as a regression guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_handler_groups_drops_by_agent() {
    timeout(Duration::from_secs(10), async {
        // Seed a real registry with mixed counters across 2 agents and
        // 2 event types (matches the shape Phase 62 RES-01 coalescer will
        // emit — text-delta drops + the occasional finish drop).
        let registry = Arc::new(MetricsRegistry::new());
        registry.record_sse_drop("agent-a", "text-delta");
        registry.record_sse_drop("agent-a", "text-delta");
        registry.record_sse_drop("agent-a", "finish");
        for _ in 0..5 {
            registry.record_sse_drop("agent-b", "text-delta");
        }

        // Build the dashboard body via the same pure function the handler
        // `api_health_dashboard(` uses. No gateway state extraction needed:
        // the handler itself is a one-liner `Json(build_dashboard_body(...))`
        // so testing this function IS testing the handler's payload.
        let body = build_dashboard_body(&registry);

        // 1. Top-level version.
        assert_eq!(body["version"], "0.19.0", "version field must be 0.19.0");

        // 2. `sse_events_dropped_total` must be an object (nested shape).
        let map = body["sse_events_dropped_total"]
            .as_object()
            .expect("sse_events_dropped_total must be a JSON object (nested)");

        // 3. Nested grouping: {agent: {event_type: count}}.
        assert_eq!(
            map["agent-a"]["text-delta"].as_u64(),
            Some(2),
            "agent-a text-delta count mismatch; body: {body}"
        );
        assert_eq!(
            map["agent-a"]["finish"].as_u64(),
            Some(1),
            "agent-a finish count mismatch; body: {body}"
        );
        assert_eq!(
            map["agent-b"]["text-delta"].as_u64(),
            Some(5),
            "agent-b text-delta count mismatch; body: {body}"
        );

        // 4. Flat pair-keys MUST NOT appear (regression guard).
        assert!(
            !map.contains_key("agent-a:text-delta"),
            "must not emit flat pair-keys like 'agent-a:text-delta'; body: {body}"
        );
        assert!(
            !map.contains_key("agent-b:text-delta"),
            "must not emit flat pair-keys like 'agent-b:text-delta'; body: {body}"
        );

        // 5. Round-trip through serde_json::to_string without loss.
        let rendered = serde_json::to_string(&body).expect("serialize");
        let reparsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("reparse");
        assert_eq!(reparsed, body, "serde round-trip must be lossless");
    })
    .await
    .expect("handler test timed out");
}

// ── Phase 65 OBS-05 — ≥10 named metrics contract ──────────────────────────

/// Phase 65 OBS-05 acceptance test. Pins the dashboard JSON extension contract:
/// `build_dashboard_body_with_snapshot` must emit at least the 10 named
/// runtime fields listed in 65-CONTEXT.md (D-05) plus `version` and the
/// Phase 62 `sse_events_dropped_total` nested object. The full required set
/// is 15 keys:
///
/// ```text
/// version, active_agents, sse_streams, approval_waiters,
/// auth_rate_limiter_size, request_rate_limiter_size, stream_registry_size,
/// db_pool_total, db_pool_idle, memory_worker_heartbeat_age_secs,
/// session_timeline_table_size_bytes, uptime_secs,
/// sse_events_dropped_total, csp_violations, csp_violations_overflow
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_has_at_least_10_named_metrics() {
    timeout(Duration::from_secs(10), async {
        let registry = Arc::new(MetricsRegistry::new());
        let snapshot = DashboardSnapshot {
            active_agents: 2,
            sse_streams: 1,
            approval_waiters: 0,
            auth_rate_limiter_size: 0,
            request_rate_limiter_size: 0,
            stream_registry_size: 1,
            db_pool_total: 10,
            db_pool_idle: 8,
            memory_worker_heartbeat_age_secs: 3,
            session_timeline_table_size_bytes: 16_384,
            uptime_secs: 60,
            ..Default::default()
        };

        let body = build_dashboard_body_with_snapshot(&registry, &snapshot);
        let obj = body
            .as_object()
            .expect("dashboard body must be a JSON object");

        // Must contain all 15 named keys (12 new OBS-05 fields + 3 Phase 62/64
        // preserved fields). Assertion-per-key gives a readable failure if
        // any individual field gets dropped in a future refactor.
        let required = [
            "version",
            "active_agents",
            "sse_streams",
            "approval_waiters",
            "auth_rate_limiter_size",
            "request_rate_limiter_size",
            "stream_registry_size",
            "db_pool_total",
            "db_pool_idle",
            "memory_worker_heartbeat_age_secs",
            "session_timeline_table_size_bytes",
            "uptime_secs",
            "sse_events_dropped_total",
            "csp_violations",
            "csp_violations_overflow",
        ];
        for k in &required {
            assert!(
                obj.contains_key(*k),
                "missing required dashboard field: {k}; body = {body}"
            );
        }

        // Substantive check on the new OBS-05 fields: they must carry the
        // values passed in via `DashboardSnapshot` (not swapped / dropped).
        assert_eq!(obj["active_agents"].as_u64(), Some(2));
        assert_eq!(obj["sse_streams"].as_u64(), Some(1));
        assert_eq!(obj["stream_registry_size"].as_u64(), Some(1));
        assert_eq!(obj["db_pool_total"].as_u64(), Some(10));
        assert_eq!(obj["db_pool_idle"].as_u64(), Some(8));
        assert_eq!(obj["memory_worker_heartbeat_age_secs"].as_i64(), Some(3));
        assert_eq!(
            obj["session_timeline_table_size_bytes"].as_u64(),
            Some(16_384)
        );
        assert_eq!(obj["uptime_secs"].as_u64(), Some(60));

        // `version` is now taken from CARGO_PKG_VERSION (not the hardcoded
        // "0.19.0" string that the Phase 62 `build_dashboard_body` used).
        let version_str = obj["version"]
            .as_str()
            .expect("version must be a string");
        assert_eq!(
            version_str,
            env!("CARGO_PKG_VERSION"),
            "version must equal CARGO_PKG_VERSION"
        );

        // ≥10 top-level fields overall (redundant with the named-key check
        // above, but makes the ≥10 contract explicit in failure output).
        assert!(
            obj.len() >= 10,
            "dashboard had {} fields, expected ≥10: {:?}",
            obj.len(),
            obj.keys().collect::<Vec<_>>()
        );
    })
    .await
    .expect("dashboard ≥10-metrics test timed out");
}

/// Phase 65 OBS-05 regression pin: Phase 62 `sse_events_dropped_total`
/// nested grouping (`{agent: {event_type: count}}`) must remain byte-identical
/// under the new snapshot-aware dashboard body function. Guards against a
/// silent shape change during the Plan 04 extension.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_preserves_phase62_nested_shape() {
    timeout(Duration::from_secs(10), async {
        let registry = Arc::new(MetricsRegistry::new());
        registry.record_sse_drop("agent-a", "text-delta");
        registry.record_sse_drop("agent-a", "text-delta");
        registry.record_sse_drop("agent-b", "finish");

        let snapshot = DashboardSnapshot {
            active_agents: 0,
            sse_streams: 0,
            approval_waiters: 0,
            auth_rate_limiter_size: 0,
            request_rate_limiter_size: 0,
            stream_registry_size: 0,
            db_pool_total: 0,
            db_pool_idle: 0,
            memory_worker_heartbeat_age_secs: -1,
            session_timeline_table_size_bytes: 0,
            uptime_secs: 0,
            ..Default::default()
        };

        let body = build_dashboard_body_with_snapshot(&registry, &snapshot);
        let map = body["sse_events_dropped_total"]
            .as_object()
            .expect("sse_events_dropped_total must remain a nested JSON object");

        assert_eq!(
            map["agent-a"]["text-delta"].as_u64(),
            Some(2),
            "agent-a/text-delta count must match; body = {body}"
        );
        assert_eq!(
            map["agent-b"]["finish"].as_u64(),
            Some(1),
            "agent-b/finish count must match; body = {body}"
        );

        // Regression guard: flat pair-keys must NOT appear.
        assert!(
            !map.contains_key("agent-a:text-delta"),
            "Phase 62 nested shape broken (flat pair-keys appeared); body = {body}"
        );
    })
    .await
    .expect("phase 62 regression pin timed out");
}

/// Phase 65 OBS-05 regression pin: Phase 64 SEC-05 `csp_violations` +
/// `csp_violations_overflow` must remain byte-identical under the new
/// snapshot-aware dashboard body function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_preserves_phase64_csp_fields() {
    timeout(Duration::from_secs(10), async {
        let registry = Arc::new(MetricsRegistry::new());
        registry.record_csp_violation("script-src");
        registry.record_csp_violation("script-src");
        registry.record_csp_violation("style-src");

        let snapshot = DashboardSnapshot {
            active_agents: 0,
            sse_streams: 0,
            approval_waiters: 0,
            auth_rate_limiter_size: 0,
            request_rate_limiter_size: 0,
            stream_registry_size: 0,
            db_pool_total: 0,
            db_pool_idle: 0,
            memory_worker_heartbeat_age_secs: -1,
            session_timeline_table_size_bytes: 0,
            uptime_secs: 0,
            ..Default::default()
        };

        let body = build_dashboard_body_with_snapshot(&registry, &snapshot);

        let csp = body["csp_violations"]
            .as_object()
            .expect("csp_violations must remain a JSON object");
        assert_eq!(
            csp["script-src"].as_u64(),
            Some(2),
            "csp_violations/script-src count mismatch; body = {body}"
        );
        assert_eq!(
            csp["style-src"].as_u64(),
            Some(1),
            "csp_violations/style-src count mismatch; body = {body}"
        );

        assert_eq!(
            body["csp_violations_overflow"].as_u64(),
            Some(0),
            "csp_violations_overflow must default to 0; body = {body}"
        );
    })
    .await
    .expect("phase 64 csp regression pin timed out");
}

/// CACHE-03: four new cache-token fields are part of the dashboard contract.
/// Operators read these to verify prompt-cache hit rates without DB access.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_includes_cache_token_aggregates() {
    timeout(Duration::from_secs(10), async {
        let registry = Arc::new(MetricsRegistry::new());
        let snap = DashboardSnapshot {
            cache_read_tokens_24h: 1_234_567,
            cache_creation_tokens_24h: 89_012,
            cache_read_tokens_7d: 9_876_543,
            cache_creation_tokens_7d: 12_345,
            ..Default::default()
        };
        let body = build_dashboard_body_with_snapshot(&registry, &snap);
        let obj = body.as_object().expect("dashboard body is a JSON object");
        for key in [
            "cache_read_tokens_24h",
            "cache_creation_tokens_24h",
            "cache_read_tokens_7d",
            "cache_creation_tokens_7d",
        ] {
            assert!(obj.contains_key(key), "dashboard JSON missing key: {key}");
            assert!(
                obj[key].is_i64() || obj[key].is_u64(),
                "key {key} must be an i64-compatible JSON number"
            );
        }
        assert_eq!(obj["cache_read_tokens_24h"].as_i64(), Some(1_234_567));
        assert_eq!(obj["cache_creation_tokens_7d"].as_i64(), Some(12_345));
    })
    .await
    .expect("cache token aggregates test timed out");
}
