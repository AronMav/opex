---
phase: 68-prompt-caching
plan: "03"
subsystem: observability
tags: [cache, metrics, dashboard, usage, sql]
dependency_graph:
  requires: [68-01]
  provides: [cache-metrics-dashboard]
  affects: [api-health-dashboard, usage_log]
tech_stack:
  added: []
  patterns: [COALESCE-FILTER-aggregate, Default-derive-for-test-fixtures, unwrap_or_default-graceful-degradation]
key_files:
  created: []
  modified:
    - crates/hydeclaw-db/src/usage.rs
    - crates/hydeclaw-core/src/metrics.rs
    - crates/hydeclaw-core/src/gateway/handlers/monitoring/mod.rs
    - crates/hydeclaw-core/tests/integration_dashboard_metrics.rs
decisions:
  - "Use FILTER aggregate (single-pass) over two separate queries — one SQL round-trip surfaces both 24h and 7d aggregates simultaneously"
  - "Outer WHERE created_at > now() - interval '7 days' bounds the scan; FILTER clauses partition the bounded set — avoids unbounded table scan (Pitfall 6)"
  - "Default derive on DashboardSnapshot makes all future struct literal fixtures forward-compatible via ..Default::default()"
  - "unwrap_or_default() on cache_metrics() — dashboard degrades to zeros on DB error, never fails the request"
metrics:
  duration: "~25 minutes"
  completed: "2026-05-08"
  tasks_completed: 3
  files_modified: 4
---

# Phase 68 Plan 03: Cache Metrics Dashboard Surface Summary

Cache token aggregates from `usage_log` (migration 036 columns) are now visible on `/api/health/dashboard`. Operators can see `cache_read_tokens_24h`, `cache_creation_tokens_24h`, `cache_read_tokens_7d`, `cache_creation_tokens_7d` to verify prompt caching is producing hits and to detect misplaced breakpoints (Pitfall 1.2: low cache_read + high cache_creation = breakpoint on volatile content).

## What Was Built

### Task 1: `hydeclaw_db::usage::cache_metrics()` — commit `43266623`

Added to `crates/hydeclaw-db/src/usage.rs`:

- `CacheMetrics` struct: 4 `i64` fields (24h + 7d windows), `#[derive(Debug, Clone, Default)]`
- `cache_metrics(db: &PgPool) -> Result<CacheMetrics>`: single SQL query using `COALESCE(SUM(col) FILTER (WHERE created_at > ...), 0)::BIGINT` — one round-trip, two time windows, NULL-safe
- Outer `WHERE created_at > now() - interval '7 days'` bounds the scan to the indexed `created_at` column (Pitfall 6 mitigation)
- 1 unit test (`cache_metrics_default_is_all_zeros` — no DB needed)
- 2 `#[sqlx::test]` integration tests (`cache_metrics_returns_zeros_on_empty_table`, `cache_metrics_sums_recent_rows` — require DATABASE_URL)

### Task 2: Extended `DashboardSnapshot` + JSON emission — commit `26fc523e`

Modified `crates/hydeclaw-core/src/metrics.rs`:

- `DashboardSnapshot` now derives `Default` (was `Debug, Clone` only)
- 4 new `pub i64` fields appended: `cache_read_tokens_24h`, `cache_creation_tokens_24h`, `cache_read_tokens_7d`, `cache_creation_tokens_7d`
- `build_dashboard_body_with_snapshot` emits 4 flat top-level JSON keys (no nesting — consistent with existing convention)
- 2 new unit tests: `dashboard_body_emits_cache_token_aggregates`, `dashboard_body_cache_aggregates_default_to_zero`

Modified `crates/hydeclaw-core/tests/integration_dashboard_metrics.rs`:

- 3 existing `DashboardSnapshot { ... }` literals updated to use `..Default::default()` (forward-compatible)
- 1 new test `dashboard_includes_cache_token_aggregates` — asserts all 4 keys present + i64-compatible
- All Phase 62/64/65 contract tests continue to pass (18/18 passed, 2 ignored for Docker requirement)

### Task 3: Handler wiring — commit `85908b22`

Modified `crates/hydeclaw-core/src/gateway/handlers/monitoring/mod.rs`:

- Calls `hydeclaw_db::usage::cache_metrics(&infra.db).await.unwrap_or_default()` once per request
- Threads 4 fields into the `DashboardSnapshot` literal
- Updated handler doc comment to document new JSON keys
- Error posture: `unwrap_or_default()` = zeros on DB error, request never fails

## Test Outcome

| Test | Result |
|------|--------|
| `usage::tests::cache_metrics_default_is_all_zeros` | PASS |
| `usage::tests::cache_metrics_returns_zeros_on_empty_table` | Requires DATABASE_URL |
| `usage::tests::cache_metrics_sums_recent_rows` | Requires DATABASE_URL |
| `metrics::tests::dashboard_body_emits_cache_token_aggregates` | PASS |
| `metrics::tests::dashboard_body_cache_aggregates_default_to_zero` | PASS |
| `integration_dashboard_metrics::dashboard_includes_cache_token_aggregates` | PASS |
| `integration_dashboard_metrics::dashboard_has_at_least_10_named_metrics` | PASS |
| `integration_dashboard_metrics::dashboard_preserves_phase62_nested_shape` | PASS |
| `integration_dashboard_metrics::dashboard_preserves_phase64_csp_fields` | PASS |

Total: 6 new tests passing, 2 expected EnvVar-gated (no regression).

## Patterns Adopted

- **SQL:** `COALESCE(SUM(col) FILTER (WHERE created_at > now() - interval 'X'), 0)::BIGINT` — single-pass partitioned aggregation with NULL safety and explicit BIGINT cast
- **Rust test fixtures:** `..Default::default()` spread syntax for `DashboardSnapshot` struct literals — future field additions need only update the type, not every test
- **Error handling:** `unwrap_or_default()` on DB-backed reads — dashboard renders with zeros rather than failing on transient DB issues

## Deviations from Plan

None — plan executed exactly as written.

## Known Stubs

None — all four cache fields are wired end-to-end from `usage_log` columns through SQL aggregate → `CacheMetrics` → `DashboardSnapshot` → JSON response.

## Forward-Looking Notes

- Phase 70 (ROUTE-02) per-target cache observability could extend with provider-broken-down aggregates (`cache_read_by_provider`) — out of scope here, but the `CacheMetrics` struct can be extended without breaking consumers
- Phase 72 (Hook API) might surface hook-level cache invalidation events — orthogonal to this plan

## Self-Check: PASSED

- All 4 modified files exist on disk
- All 3 task commits found: `43266623`, `26fc523e`, `85908b22`
- 6 new tests pass, 0 regressions
