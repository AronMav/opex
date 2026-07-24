//! Phase 62 minimal metrics registry.
//!
//! AtomicU64 counters keyed by (agent, event_type). Phase 65 OBS-02 layers
//! OpenTelemetry meter wrappers on top — Phase 62 only needs raw counters
//! to back GET /api/health/dashboard and the Phase 62 RES-01 coalescer
//! drop counter. NO external dependencies (std + tracing only) on the
//! always-on path.
//!
//! Phase 64 SEC-05 (additive): CSP violation counter keyed by directive,
//! with length + cardinality caps to prevent hostile browsers from inflating
//! the map. Overflow attempts past the cap bump a single `csp_violations_overflow`
//! atomic instead of growing the map.
//!
//! Phase 65 OBS-02 / OBS-03 (additive, feature-gated on `otel`):
//!   * Three histograms — `tool_latency_seconds`, `llm_call_duration_seconds`,
//!     `db_query_duration_seconds` — recorded both into always-on
//!     `(count, sum_micros)` atomic summaries and (when `--features otel`
//!     is on) into OTel `Histogram<f64>` instruments.
//!   * One directional counter — `llm_tokens_total{direction}` — with the
//!     same split.
//!   * Label allowlist `ALLOWED_LABEL_KEYS = {agent_id, tool_name, provider,
//!     model, result}` enforced at runtime via `assert_label_allowed()`.
//!   * Runtime cardinality guard — inserting more than `MAX_UNIQUE_SERIES`
//!     distinct histogram keys panics with a diagnostic. Catches the classic
//!     "session_id became a label" mistake before it blows Prometheus memory.
//!
//! Phase 65 OBS-05 scaffolding:
//!   * `DashboardSnapshot` + `build_dashboard_body_with_snapshot()` — the
//!     monitoring handler consumes these. Owned by Plan 65-04 (adds full
//!     contract tests); introduced here to unblock the `--features otel`
//!     build which otherwise fails at the handler's `use` site.
//!
//! NOTE on dead-code warnings: the new Plan 02 record_* / snapshot_* items
//! are consumed by integration tests (`integration_cardinality_guard.rs`,
//! `integration_otel_export.rs`) and by Plan 04's extended dashboard body,
//! but the `opex-core` BINARY TARGET does not yet wire them into
//! engine.rs / db/ (that is Phase 66 REF). Applying
//! `#![allow(dead_code)]` at module scope satisfies `-D warnings` on the
//! bin target without silencing genuine dead-code in other modules.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Sampled-warn sampling rate: log 1 out of every 64 drops.
/// Keeps logs non-overwhelming under saturation (RES-02).
const DROP_WARN_SAMPLE_RATE: u64 = 64;

/// Phase 64 SEC-05: cap distinct directives to prevent unbounded growth from
/// hostile browsers cycling directive names. Covers every standard CSP directive
/// (default-src, script-src, style-src, img-src, connect-src, font-src, object-src,
/// media-src, frame-src, worker-src, manifest-src, form-action, frame-ancestors,
/// base-uri, report-uri, report-to, upgrade-insecure-requests, block-all-mixed-content,
/// require-sri-for, require-trusted-types-for, trusted-types, sandbox, plugin-types,
/// prefetch-src, navigate-to, referrer, child-src, script-src-elem, script-src-attr,
/// style-src-elem, style-src-attr, webrtc ≈ 31) with 1 slot of headroom.
pub const MAX_CSP_DIRECTIVES: usize = 32;

/// Phase 64 SEC-05: cap each directive key length — hostile browsers could otherwise
/// send multi-KB "directive" strings that bloat the counter map memory footprint.
pub const MAX_CSP_DIRECTIVE_LEN: usize = 64;

/// Phase 65 OBS-03 (CONTEXT.md D-03): the ONLY label keys that may appear on
/// histograms / counters. Any other key causes a runtime panic in
/// [`MetricsRegistry::assert_label_allowed`] — prevents high-cardinality
/// identifiers (`session_id`, `user_id`, `request_id`, stringified error
/// messages, …) from leaking into metric labels and exploding the series
/// count.
pub const ALLOWED_LABEL_KEYS: &[&str] = &[
    "agent_id", "tool_name", "provider", "model", "result",
];

/// Phase 65 OBS-03: hard cap on unique (tool_name × agent_id × provider ×
/// model × result) tuples. Past this, the registry REFUSES new series and
/// bumps `series_overflow` — graceful degradation, because telemetry must
/// never be able to crash request processing (the old behaviour `panic!`ed
/// here, aborting the process on a user-influenced label such as a YAML/MCP
/// tool name). Pinned by the `integration_cardinality_guard.rs`
/// synthetic-session test. The ceiling is large enough for realistic
/// combinations (5 agents × 20 tools × 4 providers × 5 models × 2 results
/// = 4000) but tight enough to catch label explosion from session_id /
/// user_id slipping through.
pub const MAX_UNIQUE_SERIES: usize = 10_000;

/// Tri-string key used by `tool_latency` and `llm_call_duration` histograms.
/// Extracted to satisfy clippy's `type_complexity` lint once the per-value
/// tuple was also added alongside the raw HashMap.
type TripleKey = (String, String, String);

/// Always-on `(count, sum_micros)` atomic pair held as the value of each
/// histogram map. Shared across all three histograms — extracted for
/// clippy::type_complexity.
type HistogramBucket = (AtomicU64, AtomicU64);

/// Central metrics registry for Phase 62 observability.
///
/// Lookup path: RwLock for keyed entry (insert on first use), AtomicU64 for
/// the hot-path increment. Reads take the RwLock in shared mode + AtomicU64
/// load. Keeps contention minimal even under 10k+ synthetic sessions.
pub struct MetricsRegistry {
    /// (agent, event_type) -> dropped counter.
    sse_events_dropped: RwLock<HashMap<(String, String), AtomicU64>>,
    /// Phase 64 SEC-05: directive -> violation count. Cardinality capped at
    /// `MAX_CSP_DIRECTIVES` (see `record_csp_violation`). Keys are truncated
    /// to `MAX_CSP_DIRECTIVE_LEN` before storage.
    csp_violations_total: RwLock<HashMap<String, AtomicU64>>,
    /// Phase 64 SEC-05: number of attempts to add a directive past the
    /// cardinality cap. A non-zero value signals abuse and should trigger
    /// operator attention.
    csp_violations_overflow: AtomicU64,

    // ── Phase 65 OBS-02: histograms (always-on `(count, sum_micros)`) ──
    /// (tool_name, agent_id, result) → (count, sum_micros). Hot-path
    /// summary for `tool_latency_seconds`.
    tool_latency: RwLock<HashMap<TripleKey, HistogramBucket>>,
    /// (provider, model, result) → (count, sum_micros). Hot-path summary for
    /// `llm_call_duration_seconds`.
    llm_call_duration: RwLock<HashMap<TripleKey, HistogramBucket>>,
    /// (result,) → (count, sum_micros). `tool_name` intentionally NOT in
    /// the DB key namespace (SQL templates would explode cardinality) —
    /// just the outcome.
    db_query_duration: RwLock<HashMap<String, HistogramBucket>>,
    /// direction ∈ {"prompt","completion"} → running total tokens. Same
    /// shape the feature-gated OTel `Counter<u64>` uses.
    llm_tokens_total: RwLock<HashMap<String, AtomicU64>>,
    /// Running count of unique series inserted across all three histograms.
    /// Checked against [`MAX_UNIQUE_SERIES`] on every new-key insert.
    unique_series: AtomicU64,
    /// Number of new series REFUSED because the cardinality cap
    /// ([`MAX_UNIQUE_SERIES`]) was reached. A non-zero value signals label
    /// explosion (e.g. session_id / user_id leaking into labels) and should
    /// trigger operator attention — mirrors `csp_violations_overflow`.
    series_overflow: AtomicU64,

    /// (provider, kind) → counter. `kind` ∈
    /// {"connect","request","inactivity","max_duration"}. Incremented by
    /// `RoutingProvider::handle_provider_error` when an `LlmCallError` of
    /// that variant is classified. Cardinality is bounded by the finite
    /// provider set × 4 kinds; no runtime guard is required.
    llm_timeout_total: RwLock<HashMap<(String, String), AtomicU64>>,
    /// (from_connection, to_connection, reason) → counter. Incremented
    /// once per failover decision inside `RoutingProvider` — i.e. whenever
    /// `handle_provider_error` returns `true` and the caller proceeds to
    /// the next route. `reason` is a short stable token such as
    /// "inactivity", "request_timeout", "connect_timeout", "max_duration",
    /// "5xx", "network", "schema_pre_stream", or "untyped".
    llm_failover_total: RwLock<HashMap<(String, String, String), AtomicU64>>,

    /// (agent, event) → counter for autonomous-resumption lifecycle events:
    /// cron-goal re-drive and interactive-`/goal` crash notification. `event` is
    /// drawn from a small fixed vocabulary (e.g. "cron_redrive_started",
    /// "cron_redrive_skipped_live", "cron_redrive_claim_raced",
    /// "cron_redrive_list_failed", "interactive_goal_notified"). Cardinality is
    /// bounded by the finite agent set × that vocabulary — no runtime guard
    /// needed. Raw counter (no OTel mirror), like `sse_events_dropped`.
    redrive_events: RwLock<HashMap<(String, String), AtomicU64>>,

    /// Feature-gated OTel instruments. Populated by
    /// [`MetricsRegistry::install_otel_instruments`] after the global
    /// `MeterProvider` is set. `None` on `--no-default-features`.
    #[cfg(feature = "otel")]
    otel_instruments: std::sync::OnceLock<OtelInstruments>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            sse_events_dropped: RwLock::new(HashMap::new()),
            csp_violations_total: RwLock::new(HashMap::new()),
            csp_violations_overflow: AtomicU64::new(0),
            tool_latency: RwLock::new(HashMap::new()),
            llm_call_duration: RwLock::new(HashMap::new()),
            db_query_duration: RwLock::new(HashMap::new()),
            llm_tokens_total: RwLock::new(HashMap::new()),
            unique_series: AtomicU64::new(0),
            series_overflow: AtomicU64::new(0),
            llm_timeout_total: RwLock::new(HashMap::new()),
            llm_failover_total: RwLock::new(HashMap::new()),
            redrive_events: RwLock::new(HashMap::new()),
            #[cfg(feature = "otel")]
            otel_instruments: std::sync::OnceLock::new(),
        }
    }

    /// Record a dropped SSE event. Safe to call from any task.
    /// Emits sampled warn log every 64th drop per (agent, event_type).
    pub fn record_sse_drop(&self, agent: &str, event_type: &str) {
        // Fast path: key already exists, grab shared read lock + atomic inc.
        {
            let read = self.sse_events_dropped.read().expect("metrics RwLock poisoned");
            if let Some(counter) = read.get(&(agent.to_string(), event_type.to_string())) {
                let prev = counter.fetch_add(1, Ordering::Relaxed);
                let new_count = prev.wrapping_add(1);
                if new_count.is_multiple_of(DROP_WARN_SAMPLE_RATE) {
                    tracing::warn!(
                        agent = %agent,
                        event_type = %event_type,
                        total = new_count,
                        "sse event drop (sampled 1/{})",
                        DROP_WARN_SAMPLE_RATE
                    );
                }
                return;
            }
        }
        // Slow path: insert new key under write lock.
        let mut write = self.sse_events_dropped.write().expect("metrics RwLock poisoned");
        let counter = write
            .entry((agent.to_string(), event_type.to_string()))
            .or_insert_with(|| AtomicU64::new(0));
        let prev = counter.fetch_add(1, Ordering::Relaxed);
        let new_count = prev.wrapping_add(1);
        if new_count.is_multiple_of(DROP_WARN_SAMPLE_RATE) {
            tracing::warn!(
                agent = %agent,
                event_type = %event_type,
                total = new_count,
                "sse event drop (sampled 1/{})",
                DROP_WARN_SAMPLE_RATE
            );
        }
    }

    /// Snapshot all dropped-event counters. Used by /api/health/dashboard.
    pub fn snapshot_sse_drops(&self) -> HashMap<(String, String), u64> {
        let read = self.sse_events_dropped.read().expect("metrics RwLock poisoned");
        read.iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }

    /// Record an autonomous-resumption lifecycle event (`event`) for `agent`.
    /// Safe to call from any task. See the `redrive_events` field doc for the
    /// event vocabulary. Use `"-"` as `agent` when no agent is in scope (e.g. a
    /// failed `list_redrivable` query).
    pub fn record_redrive_event(&self, agent: &str, event: &str) {
        let key = (agent.to_string(), event.to_string());
        // Fast path: existing key → shared read lock + atomic inc.
        {
            let read = self.redrive_events.read().expect("metrics RwLock poisoned");
            if let Some(counter) = read.get(&key) {
                counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        // Slow path: insert new key under write lock (re-check after upgrade).
        let mut write = self.redrive_events.write().expect("metrics RwLock poisoned");
        write
            .entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot all autonomous-resumption lifecycle counters, keyed by
    /// `(agent, event)`. Used by `/api/health/dashboard`.
    pub fn snapshot_redrive_events(&self) -> HashMap<(String, String), u64> {
        let read = self.redrive_events.read().expect("metrics RwLock poisoned");
        read.iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }

    // ── Phase 64 SEC-05 — CSP violations counter ────────────────────────

    /// Record a single CSP violation for the given directive.
    ///
    /// Defensive policy:
    ///   * Directive keys longer than [`MAX_CSP_DIRECTIVE_LEN`] are truncated.
    ///   * Existing keys always increment — even if the map is at capacity.
    ///   * New keys are rejected once the map reaches [`MAX_CSP_DIRECTIVES`]
    ///     entries; the rejection increments `csp_violations_overflow` so
    ///     operators see the abuse signal in the dashboard.
    ///
    /// Truncation happens on a byte boundary via `char` iteration so we never
    /// split UTF-8 mid-sequence, even though browsers normally only send ASCII
    /// directive names.
    pub fn record_csp_violation(&self, directive: &str) {
        let key: String = if directive.len() > MAX_CSP_DIRECTIVE_LEN {
            let mut truncated = String::with_capacity(MAX_CSP_DIRECTIVE_LEN);
            for ch in directive.chars() {
                if truncated.len() + ch.len_utf8() > MAX_CSP_DIRECTIVE_LEN {
                    break;
                }
                truncated.push(ch);
            }
            truncated
        } else {
            directive.to_string()
        };

        // Fast path: key already present → bump under a read lock.
        {
            let read = self
                .csp_violations_total
                .read()
                .expect("csp RwLock poisoned");
            if let Some(counter) = read.get(&key) {
                counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }

        // Slow path: upgrade to write lock, enforce cardinality cap.
        let mut write = self
            .csp_violations_total
            .write()
            .expect("csp RwLock poisoned");
        // Re-check after re-acquiring (another writer may have inserted).
        if let Some(counter) = write.get(&key) {
            counter.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if write.len() >= MAX_CSP_DIRECTIVES {
            self.csp_violations_overflow.fetch_add(1, Ordering::Relaxed);
            return;
        }
        write
            .entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Read current count for a specific directive (0 if absent).
    /// Test-facing accessor (used by `integration_csp_report.rs`).
    pub fn csp_violations_total_count(&self, directive: &str) -> u64 {
        let read = self
            .csp_violations_total
            .read()
            .expect("csp RwLock poisoned");
        read.get(directive)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Number of distinct directives currently stored (useful for cap tests).
    /// Test-facing accessor.
    pub fn csp_violations_map_len(&self) -> usize {
        let read = self
            .csp_violations_total
            .read()
            .expect("csp RwLock poisoned");
        read.len()
    }

    /// Snapshot all stored directive keys (test-facing; allocates a Vec).
    pub fn csp_violations_keys_snapshot(&self) -> Vec<String> {
        let read = self
            .csp_violations_total
            .read()
            .expect("csp RwLock poisoned");
        read.keys().cloned().collect()
    }

    /// Snapshot all CSP violation counts as a `{directive: count}` map.
    pub fn snapshot_csp_violations(&self) -> HashMap<String, u64> {
        let read = self
            .csp_violations_total
            .read()
            .expect("csp RwLock poisoned");
        read.iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }

    /// Overflow counter — bumped every time a new directive is rejected
    /// because the map is already at [`MAX_CSP_DIRECTIVES`] entries.
    pub fn csp_violations_overflow_count(&self) -> u64 {
        self.csp_violations_overflow.load(Ordering::Relaxed)
    }

    // ── Phase 65 OBS-02 / OBS-03 — histograms + allowlist + cardinality ──

    /// Panics if `key` is not in [`ALLOWED_LABEL_KEYS`]. Public so the
    /// dashboard / audit paths can defensively re-check, and so
    /// `integration_cardinality_guard.rs` can pin the runtime contract
    /// via a `#[should_panic]` test.
    pub fn assert_label_allowed(key: &str) {
        if !ALLOWED_LABEL_KEYS.contains(&key) {
            panic!("label key not in allowlist: {key}");
        }
    }

    /// Number of unique series currently tracked across all histograms.
    /// Test-facing accessor (used by `integration_cardinality_guard.rs`).
    pub fn unique_series_count(&self) -> u64 {
        self.unique_series.load(Ordering::Relaxed)
    }

    /// Number of new series refused after the cardinality cap was reached.
    /// Operator-facing signal of label explosion (mirrors
    /// `csp_violations_overflow_count`).
    pub fn series_overflow_count(&self) -> u64 {
        self.series_overflow.load(Ordering::Relaxed)
    }

    /// Cardinality guard. Called on every new-key insert across the
    /// histograms. Returns `false` once the running count exceeds
    /// [`MAX_UNIQUE_SERIES`] so the caller DROPS the new series instead of
    /// growing the map without bound.
    ///
    /// Telemetry must degrade gracefully — it must never be able to crash
    /// request processing. The old behaviour `panic!`ed here, which could
    /// abort the process on a user-influenced label (a YAML/MCP tool name is
    /// operator-controlled cardinality). Overflow is counted in
    /// `series_overflow` and warn-logged (sampled) for operator attention.
    fn try_reserve_series(&self) -> bool {
        let n = self.unique_series.fetch_add(1, Ordering::Relaxed) + 1;
        if (n as usize) <= MAX_UNIQUE_SERIES {
            return true;
        }
        // Over the cap — undo the reservation so `unique_series` keeps
        // counting only ACCEPTED series (stays ≤ cap), then record the
        // refusal for operator visibility.
        self.unique_series.fetch_sub(1, Ordering::Relaxed);
        let dropped = self.series_overflow.fetch_add(1, Ordering::Relaxed) + 1;
        if dropped == 1 || dropped.is_multiple_of(DROP_WARN_SAMPLE_RATE) {
            tracing::warn!(
                series = n,
                cap = MAX_UNIQUE_SERIES,
                dropped_total = dropped,
                allowed_labels = ?ALLOWED_LABEL_KEYS,
                "metrics cardinality cap reached — refusing new series \
                 (check that no code path passes session_id / user_id / request_id as a label)"
            );
        }
        false
    }

    /// Record tool latency. `result` SHOULD be a bounded-cardinality value
    /// — typically `"ok"`, `"error"`, or `"timeout"` — NOT the error
    /// message body (that would explode cardinality).
    ///
    /// Always-on: bumps `(count, sum_micros)` in the in-process summary.
    /// Feature-gated (`otel`): also records on the OTel `f64_histogram`
    /// with seconds resolution, labels filtered to `ALLOWED_LABEL_KEYS`.
    pub fn record_tool_latency(
        &self,
        tool_name: &str,
        agent_id: &str,
        result: &str,
        d: Duration,
    ) {
        let micros = d.as_micros() as u64;
        let key = (
            tool_name.to_string(),
            agent_id.to_string(),
            result.to_string(),
        );

        // Fast path: existing key — atomic bump under read lock.
        {
            let read = self.tool_latency.read().expect("tool_latency RwLock poisoned");
            if let Some((count, sum)) = read.get(&key) {
                count.fetch_add(1, Ordering::Relaxed);
                sum.fetch_add(micros, Ordering::Relaxed);
                self.record_tool_latency_otel(tool_name, agent_id, result, d);
                return;
            }
        }

        // Slow path: insert new key. Cardinality guard runs inside — MUST
        // happen BEFORE the insert so a panic does not leak half-baked
        // state into the summary map.
        let mut write = self.tool_latency.write().expect("tool_latency RwLock poisoned");
        if let Some((count, sum)) = write.get(&key) {
            count.fetch_add(1, Ordering::Relaxed);
            sum.fetch_add(micros, Ordering::Relaxed);
            drop(write);
            self.record_tool_latency_otel(tool_name, agent_id, result, d);
            return;
        }
        if !self.try_reserve_series() {
            return;
        }
        write.insert(key, (AtomicU64::new(1), AtomicU64::new(micros)));
        drop(write);
        self.record_tool_latency_otel(tool_name, agent_id, result, d);
    }

    /// Record LLM call duration. `provider` and `model` come from the
    /// provider registry (bounded — a few dozen max). `result` is
    /// `"ok"`/`"error"`/`"timeout"`.
    pub fn record_llm_call_duration(
        &self,
        provider: &str,
        model: &str,
        result: &str,
        d: Duration,
    ) {
        let micros = d.as_micros() as u64;
        let key = (
            provider.to_string(),
            model.to_string(),
            result.to_string(),
        );

        {
            let read = self
                .llm_call_duration
                .read()
                .expect("llm_call_duration RwLock poisoned");
            if let Some((count, sum)) = read.get(&key) {
                count.fetch_add(1, Ordering::Relaxed);
                sum.fetch_add(micros, Ordering::Relaxed);
                self.record_llm_call_duration_otel(provider, model, result, d);
                return;
            }
        }

        let mut write = self
            .llm_call_duration
            .write()
            .expect("llm_call_duration RwLock poisoned");
        if let Some((count, sum)) = write.get(&key) {
            count.fetch_add(1, Ordering::Relaxed);
            sum.fetch_add(micros, Ordering::Relaxed);
            drop(write);
            self.record_llm_call_duration_otel(provider, model, result, d);
            return;
        }
        if !self.try_reserve_series() {
            return;
        }
        write.insert(key, (AtomicU64::new(1), AtomicU64::new(micros)));
        drop(write);
        self.record_llm_call_duration_otel(provider, model, result, d);
    }

    /// Record DB query duration. Keyed by `result` only (SQL templates are
    /// templated strings — including them would explode cardinality).
    pub fn record_db_query_duration(&self, result: &str, d: Duration) {
        let micros = d.as_micros() as u64;
        let key = result.to_string();

        {
            let read = self
                .db_query_duration
                .read()
                .expect("db_query_duration RwLock poisoned");
            if let Some((count, sum)) = read.get(&key) {
                count.fetch_add(1, Ordering::Relaxed);
                sum.fetch_add(micros, Ordering::Relaxed);
                self.record_db_query_duration_otel(result, d);
                return;
            }
        }

        let mut write = self
            .db_query_duration
            .write()
            .expect("db_query_duration RwLock poisoned");
        if let Some((count, sum)) = write.get(&key) {
            count.fetch_add(1, Ordering::Relaxed);
            sum.fetch_add(micros, Ordering::Relaxed);
            drop(write);
            self.record_db_query_duration_otel(result, d);
            return;
        }
        if !self.try_reserve_series() {
            return;
        }
        write.insert(key, (AtomicU64::new(1), AtomicU64::new(micros)));
        drop(write);
        self.record_db_query_duration_otel(result, d);
    }

    /// Record LLM token usage. `direction` MUST be `"prompt"` or
    /// `"completion"` — debug-asserted; other values still accumulate but
    /// the debug_assert fires in dev/test builds.
    pub fn record_llm_tokens(&self, n: u64, direction: &str) {
        debug_assert!(
            direction == "prompt" || direction == "completion",
            "llm_tokens direction must be 'prompt' or 'completion', got {direction:?}"
        );
        let key = direction.to_string();

        {
            let read = self
                .llm_tokens_total
                .read()
                .expect("llm_tokens_total RwLock poisoned");
            if let Some(counter) = read.get(&key) {
                counter.fetch_add(n, Ordering::Relaxed);
                self.record_llm_tokens_otel(n, direction);
                return;
            }
        }

        let mut write = self
            .llm_tokens_total
            .write()
            .expect("llm_tokens_total RwLock poisoned");
        if let Some(counter) = write.get(&key) {
            counter.fetch_add(n, Ordering::Relaxed);
            drop(write);
            self.record_llm_tokens_otel(n, direction);
            return;
        }
        // llm_tokens_total shares the unique-series budget — each new
        // direction is a new series. Bounded to 2 in practice (prompt,
        // completion) but we count it for safety.
        if !self.try_reserve_series() {
            return;
        }
        write.insert(key, AtomicU64::new(n));
        drop(write);
        self.record_llm_tokens_otel(n, direction);
    }

    /// Snapshot tool_latency summary as `{(tool, agent, result): (count, sum_micros)}`.
    pub fn snapshot_tool_latency_summary(
        &self,
    ) -> HashMap<(String, String, String), (u64, u64)> {
        let read = self.tool_latency.read().expect("tool_latency RwLock poisoned");
        read.iter()
            .map(|(k, (c, s))| (k.clone(), (c.load(Ordering::Relaxed), s.load(Ordering::Relaxed))))
            .collect()
    }

    /// Snapshot llm_call_duration summary as `{(provider, model, result): (count, sum_micros)}`.
    pub fn snapshot_llm_call_duration_summary(
        &self,
    ) -> HashMap<(String, String, String), (u64, u64)> {
        let read = self
            .llm_call_duration
            .read()
            .expect("llm_call_duration RwLock poisoned");
        read.iter()
            .map(|(k, (c, s))| (k.clone(), (c.load(Ordering::Relaxed), s.load(Ordering::Relaxed))))
            .collect()
    }

    /// Snapshot db_query_duration summary as `{result: (count, sum_micros)}`.
    pub fn snapshot_db_query_duration_summary(&self) -> HashMap<String, (u64, u64)> {
        let read = self
            .db_query_duration
            .read()
            .expect("db_query_duration RwLock poisoned");
        read.iter()
            .map(|(k, (c, s))| (k.clone(), (c.load(Ordering::Relaxed), s.load(Ordering::Relaxed))))
            .collect()
    }

    /// Snapshot llm_tokens_total as `{direction: total}`.
    pub fn snapshot_llm_tokens_total(&self) -> HashMap<String, u64> {
        let read = self
            .llm_tokens_total
            .read()
            .expect("llm_tokens_total RwLock poisoned");
        read.iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }

    // ── Feature-gated OTel emitter helpers ──────────────────────────────
    // Each `record_*` method calls the matching `*_otel` helper after the
    // always-on bump. When `--features otel` is off, these helpers are
    // no-ops (inlined to nothing by the optimizer).

    #[cfg(not(feature = "otel"))]
    fn record_tool_latency_otel(
        &self,
        _tool_name: &str,
        _agent_id: &str,
        _result: &str,
        _d: Duration,
    ) {
    }

    #[cfg(feature = "otel")]
    fn record_tool_latency_otel(
        &self,
        tool_name: &str,
        agent_id: &str,
        result: &str,
        d: Duration,
    ) {
        if let Some(inst) = self.otel_instruments.get() {
            use opentelemetry::KeyValue;
            inst.tool_latency.record(
                d.as_secs_f64(),
                &[
                    KeyValue::new("tool_name", tool_name.to_string()),
                    KeyValue::new("agent_id", agent_id.to_string()),
                    KeyValue::new("result", result.to_string()),
                ],
            );
        }
    }

    #[cfg(not(feature = "otel"))]
    fn record_llm_call_duration_otel(
        &self,
        _provider: &str,
        _model: &str,
        _result: &str,
        _d: Duration,
    ) {
    }

    #[cfg(feature = "otel")]
    fn record_llm_call_duration_otel(
        &self,
        provider: &str,
        model: &str,
        result: &str,
        d: Duration,
    ) {
        if let Some(inst) = self.otel_instruments.get() {
            use opentelemetry::KeyValue;
            inst.llm_call_duration.record(
                d.as_secs_f64(),
                &[
                    KeyValue::new("provider", provider.to_string()),
                    KeyValue::new("model", model.to_string()),
                    KeyValue::new("result", result.to_string()),
                ],
            );
        }
    }

    #[cfg(not(feature = "otel"))]
    fn record_db_query_duration_otel(&self, _result: &str, _d: Duration) {}

    #[cfg(feature = "otel")]
    fn record_db_query_duration_otel(&self, result: &str, d: Duration) {
        if let Some(inst) = self.otel_instruments.get() {
            use opentelemetry::KeyValue;
            inst.db_query_duration.record(
                d.as_secs_f64(),
                &[KeyValue::new("result", result.to_string())],
            );
        }
    }

    #[cfg(not(feature = "otel"))]
    fn record_llm_tokens_otel(&self, _n: u64, _direction: &str) {}

    #[cfg(feature = "otel")]
    fn record_llm_tokens_otel(&self, n: u64, direction: &str) {
        if let Some(inst) = self.otel_instruments.get() {
            use opentelemetry::KeyValue;
            inst.llm_tokens_total
                .add(n, &[KeyValue::new("direction", direction.to_string())]);
        }
    }
}

// ── Feature-gated OTel instrument wiring ────────────────────────────────
// Lives at module bottom so the always-on path above stays self-contained.

#[cfg(feature = "otel")]
struct OtelInstruments {
    tool_latency: opentelemetry::metrics::Histogram<f64>,
    llm_call_duration: opentelemetry::metrics::Histogram<f64>,
    db_query_duration: opentelemetry::metrics::Histogram<f64>,
    llm_tokens_total: opentelemetry::metrics::Counter<u64>,
}

#[cfg(feature = "otel")]
impl std::fmt::Debug for OtelInstruments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtelInstruments")
            .field("tool_latency", &"Histogram<f64> tool_latency_seconds")
            .field("llm_call_duration", &"Histogram<f64> llm_call_duration_seconds")
            .field("db_query_duration", &"Histogram<f64> db_query_duration_seconds")
            .field("llm_tokens_total", &"Counter<u64> llm_tokens_total")
            .finish()
    }
}

#[cfg(feature = "otel")]
impl MetricsRegistry {
    /// Install OTel instruments on this registry. Must be called once,
    /// from `main.rs::init_tracing()`, AFTER the global `MeterProvider`
    /// is set via `opentelemetry::global::set_meter_provider()`.
    ///
    /// Idempotent: subsequent calls are no-ops (the underlying
    /// `OnceLock::set` fails silently when already initialized).
    pub fn install_otel_instruments(&self) {
        use opentelemetry::global;
        let meter = global::meter("opex-core");
        let inst = OtelInstruments {
            tool_latency: meter
                .f64_histogram("tool_latency_seconds")
                .with_unit("s")
                .build(),
            llm_call_duration: meter
                .f64_histogram("llm_call_duration_seconds")
                .with_unit("s")
                .build(),
            db_query_duration: meter
                .f64_histogram("db_query_duration_seconds")
                .with_unit("s")
                .build(),
            llm_tokens_total: meter.u64_counter("llm_tokens_total").build(),
        };
        let _ = self.otel_instruments.set(inst);
    }
}

impl MetricsRegistry {
    // ── LLM timeout / failover counter APIs ──────────────────────────────

    /// Bump `llm_timeout_total{provider, kind}`. `kind` SHOULD be one of
    /// the four bounded-cardinality tokens: `"connect"`, `"request"`,
    /// `"inactivity"`, `"max_duration"`. Debug-asserted.
    pub fn record_llm_timeout(&self, provider: &str, kind: &str) {
        debug_assert!(
            matches!(kind, "connect" | "request" | "inactivity" | "max_duration"),
            "llm_timeout kind must be one of connect|request|inactivity|max_duration, got {kind:?}"
        );
        let key = (provider.to_string(), kind.to_string());
        {
            let read = self
                .llm_timeout_total
                .read()
                .expect("llm_timeout_total RwLock poisoned");
            if let Some(counter) = read.get(&key) {
                counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        let mut write = self
            .llm_timeout_total
            .write()
            .expect("llm_timeout_total RwLock poisoned");
        write
            .entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Bump `llm_failover_total{from_connection, to_connection, reason}`.
    /// Recorded at the decision point where `RoutingProvider` proceeds to
    /// the next route. `reason` is a short stable token (see struct field
    /// docstring).
    pub fn record_llm_failover(&self, from_connection: &str, to_connection: &str, reason: &str) {
        let key = (
            from_connection.to_string(),
            to_connection.to_string(),
            reason.to_string(),
        );
        {
            let read = self
                .llm_failover_total
                .read()
                .expect("llm_failover_total RwLock poisoned");
            if let Some(counter) = read.get(&key) {
                counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        let mut write = self
            .llm_failover_total
            .write()
            .expect("llm_failover_total RwLock poisoned");
        write
            .entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot `llm_timeout_total` as `{(provider, kind): count}`.
    pub fn snapshot_llm_timeout_total(&self) -> HashMap<(String, String), u64> {
        let read = self
            .llm_timeout_total
            .read()
            .expect("llm_timeout_total RwLock poisoned");
        read.iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }

    /// Snapshot `llm_failover_total` as `{(from, to, reason): count}`.
    pub fn snapshot_llm_failover_total(
        &self,
    ) -> HashMap<(String, String, String), u64> {
        let read = self
            .llm_failover_total
            .read()
            .expect("llm_failover_total RwLock poisoned");
        read.iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }
}

// ── Process-wide `MetricsRegistry` handle ──────────────────────────────
// Used by `RoutingProvider::handle_provider_error`, which does not receive
// the registry through its constructor (the routing chain is built deep
// inside `create_routing_provider` without `AppState` in scope). The
// gateway sets this OnceLock immediately after it constructs the shared
// `Arc<MetricsRegistry>` so every downstream path sees the same counters.

static GLOBAL_METRICS: OnceLock<Arc<MetricsRegistry>> = OnceLock::new();

/// Install the process-wide metrics registry. Idempotent — subsequent
/// calls are no-ops (first-writer-wins via `OnceLock::set`). Called once
/// from `main.rs` after the shared `Arc<MetricsRegistry>` is built for
/// `InfraServices`.
pub fn install_global(registry: Arc<MetricsRegistry>) {
    let _ = GLOBAL_METRICS.set(registry);
}

/// Return the process-wide metrics registry, if one has been installed.
/// Returns `None` before `install_global` is called (e.g. in unit tests
/// or during very early startup) so call sites must tolerate absence.
pub fn global() -> Option<&'static Arc<MetricsRegistry>> {
    GLOBAL_METRICS.get()
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `GET /api/health/dashboard` response body from a `MetricsRegistry`.
///
/// Pure flat→nested transformation: `snapshot_sse_drops()` returns
/// `HashMap<(agent, event_type), u64>` (flat), and this function groups it
/// into `{agent: {event_type: count}}` (nested) using `BTreeMap` for stable
/// key ordering in serialized JSON.
///
/// Used by `gateway::handlers::monitoring::api_health_dashboard` as the
/// single source of truth for the dashboard JSON shape.  Exposed on the
/// library surface so integration tests (`integration_dashboard_metrics.rs`)
/// can pin the nested-grouping contract without reaching into the gateway
/// handler subtree.
///
/// Returns a JSON object of the form:
/// ```json
/// {
///   "version": "0.19.0",
///   "sse_events_dropped_total": { "<agent>": { "<event_type>": <count> } }
/// }
/// ```
/// Phase 65 OBS-05 extends with additional fields (active_agents, DB pool
/// stats, …); clients MUST treat unknown top-level fields as opaque.
pub fn build_dashboard_body(registry: &MetricsRegistry) -> serde_json::Value {
    use std::collections::BTreeMap;

    let drops = registry.snapshot_sse_drops();
    // Flat (agent, event_type) → nested {agent: {event_type: count}}.
    let mut by_agent: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for ((agent, event_type), count) in drops {
        by_agent.entry(agent).or_default().insert(event_type, count);
    }

    // Phase 64 SEC-05: CSP violation counter (additive field; pre-existing
    // dashboard consumers treat unknown keys as opaque — see RES-02 doc).
    let csp_violations: BTreeMap<String, u64> = registry
        .snapshot_csp_violations()
        .into_iter()
        .collect();

    serde_json::json!({
        "version": "0.19.0",
        "sse_events_dropped_total": by_agent,
        "csp_violations": csp_violations,
        "csp_violations_overflow": registry.csp_violations_overflow_count(),
        "series_overflow": registry.series_overflow_count(),
    })
}

// ── Phase 65 OBS-05 scaffolding (consumed by Plan 65-04) ──────────────

/// Runtime snapshot of cluster-level counters the `MetricsRegistry` itself
/// does not own. Collected by the `/api/health/dashboard` handler from
/// `AppState` clusters (DB pool, agent map, SSE stream registry, approval
/// waiters, rate limiters, status monitor) and passed into
/// [`build_dashboard_body_with_snapshot`] to populate the extended dashboard
/// JSON body.
///
/// Isolation boundary: keeping the cluster reads in the handler layer (not
/// in `metrics.rs`) preserves the `metrics` module's leaf-discipline status
/// — it has zero `crate::*` dependencies and stays re-exportable via the
/// `opex_core::metrics` lib facade (see `src/lib.rs` 10-module cap).
///
/// Plan 65-04 owns the full contract test
/// (`dashboard_has_at_least_10_named_metrics`). This struct is introduced
/// in Plan 65-02 ONLY because the monitoring handler already references it
/// and the default/feature builds would otherwise fail to compile.
#[derive(Debug, Clone, Default)]
pub struct DashboardSnapshot {
    /// Number of running agents (`AgentCore.map.len()`).
    pub active_agents: u64,
    /// Active SSE streams currently registered.
    pub sse_streams: u64,
    /// Pending approval waiters (in-memory oneshot senders awaiting resolve).
    pub approval_waiters: u64,
    /// Entries currently held in the `AuthRateLimiter` state map.
    pub auth_rate_limiter_size: u64,
    /// Entries currently held in the `RequestRateLimiter` state map.
    pub request_rate_limiter_size: u64,
    /// Alias of `sse_streams` for clarity in dashboards (both fields are
    /// emitted so UIs can pick whichever label fits).
    pub stream_registry_size: u64,
    /// sqlx PgPool configured pool size (`pool.size()`).
    pub db_pool_total: u64,
    /// sqlx PgPool idle connection count (`pool.num_idle()`).
    pub db_pool_idle: u64,
    /// Age in seconds of the latest `memory_tasks` row, or `-1` if the
    /// heartbeat is unknown / the table is empty.
    pub memory_worker_heartbeat_age_secs: i64,
    /// `pg_total_relation_size('session_timeline')` — Postgres-reported
    /// on-disk size of the session timeline table, in bytes.
    pub session_timeline_table_size_bytes: u64,
    /// Process uptime in whole seconds (`StatusMonitor.started_at.elapsed()`).
    pub uptime_secs: u64,
    /// CACHE-03: SUM of `cache_read_tokens` from `usage_log` over the last 24 hours.
    /// 0 on empty table or DB error (graceful degradation in dashboard handler).
    pub cache_read_tokens_24h: i64,
    /// CACHE-03: SUM of `cache_creation_tokens` from `usage_log` over the last 24 hours.
    pub cache_creation_tokens_24h: i64,
    /// CACHE-03: SUM of `cache_read_tokens` from `usage_log` over the last 7 days.
    pub cache_read_tokens_7d: i64,
    /// CACHE-03: SUM of `cache_creation_tokens` from `usage_log` over the last 7 days.
    pub cache_creation_tokens_7d: i64,
}

/// Build the `/api/health/dashboard` response body, extending the Phase 62
/// payload with Phase 65 OBS-05 cluster-level runtime fields.
///
/// Contract (additive extension — Plan 04 success criteria):
///   * Every Phase 62 field from [`build_dashboard_body`] remains present
///     and byte-identical in shape (`sse_events_dropped_total` stays nested,
///     `csp_violations` + `csp_violations_overflow` unchanged).
///   * `version` is upgraded from the Phase 62 hardcoded `"0.19.0"` string
///     to the live `env!("CARGO_PKG_VERSION")` — single source of truth
///     matches `Cargo.toml` so a version bump does not leave the dashboard
///     stale.
///   * Adds the 11 numeric cluster fields from [`DashboardSnapshot`] as
///     flat top-level JSON numbers (no nesting — scraping tools parse
///     `body.active_agents` directly).
///
/// Clients MUST continue to treat unknown top-level fields as opaque so
/// future OBS phases can add more signals without breaking consumers.
pub fn build_dashboard_body_with_snapshot(
    registry: &MetricsRegistry,
    snap: &DashboardSnapshot,
) -> serde_json::Value {
    use std::collections::BTreeMap;

    let drops = registry.snapshot_sse_drops();
    let mut by_agent: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for ((agent, event_type), count) in drops {
        by_agent.entry(agent).or_default().insert(event_type, count);
    }
    let csp_violations: BTreeMap<String, u64> = registry
        .snapshot_csp_violations()
        .into_iter()
        .collect();

    // Expose llm_timeout_total + llm_failover_total as nested JSON for the health dashboard.
    // `llm_timeout_total`: {provider: {kind: count}}
    let mut llm_timeouts: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for ((provider, kind), count) in registry.snapshot_llm_timeout_total() {
        llm_timeouts.entry(provider).or_default().insert(kind, count);
    }
    // `llm_failover_total`: [{from, to, reason, count}] — flat list so
    // (from, to, reason) triples remain addressable without deep nesting.
    let mut llm_failovers: Vec<serde_json::Value> = registry
        .snapshot_llm_failover_total()
        .into_iter()
        .map(|((from, to, reason), count)| {
            serde_json::json!({
                "from": from,
                "to": to,
                "reason": reason,
                "count": count,
            })
        })
        .collect();
    // Stable ordering so dashboards don't flicker between calls.
    llm_failovers.sort_by(|a, b| {
        let ka = (
            a["from"].as_str().unwrap_or(""),
            a["to"].as_str().unwrap_or(""),
            a["reason"].as_str().unwrap_or(""),
        );
        let kb = (
            b["from"].as_str().unwrap_or(""),
            b["to"].as_str().unwrap_or(""),
            b["reason"].as_str().unwrap_or(""),
        );
        ka.cmp(&kb)
    });

    // Autonomous-resumption lifecycle counters: {agent: {event: count}}.
    let mut redrive_events: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for ((agent, event), count) in registry.snapshot_redrive_events() {
        redrive_events.entry(agent).or_default().insert(event, count);
    }

    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "active_agents": snap.active_agents,
        "sse_streams": snap.sse_streams,
        "approval_waiters": snap.approval_waiters,
        "auth_rate_limiter_size": snap.auth_rate_limiter_size,
        "request_rate_limiter_size": snap.request_rate_limiter_size,
        "stream_registry_size": snap.stream_registry_size,
        "db_pool_total": snap.db_pool_total,
        "db_pool_idle": snap.db_pool_idle,
        "memory_worker_heartbeat_age_secs": snap.memory_worker_heartbeat_age_secs,
        "session_timeline_table_size_bytes": snap.session_timeline_table_size_bytes,
        "uptime_secs": snap.uptime_secs,
        "sse_events_dropped_total": by_agent,
        "csp_violations": csp_violations,
        "csp_violations_overflow": registry.csp_violations_overflow_count(),
        "series_overflow": registry.series_overflow_count(),
        "llm_timeout_total": llm_timeouts,
        "llm_failover_total": llm_failovers,
        "redrive_events_total": redrive_events,
        // CACHE-03: prompt-caching aggregates from usage_log.
        "cache_read_tokens_24h": snap.cache_read_tokens_24h,
        "cache_creation_tokens_24h": snap.cache_creation_tokens_24h,
        "cache_read_tokens_7d": snap.cache_read_tokens_7d,
        "cache_creation_tokens_7d": snap.cache_creation_tokens_7d,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn new_registry_has_empty_snapshot() {
        let reg = MetricsRegistry::new();
        assert!(reg.snapshot_sse_drops().is_empty());
    }

    #[test]
    fn record_sse_drop_accumulates() {
        let reg = MetricsRegistry::new();
        for _ in 0..3 {
            reg.record_sse_drop("agent-a", "text-delta");
        }
        reg.record_sse_drop("agent-b", "finish");
        let snap = reg.snapshot_sse_drops();
        assert_eq!(snap.get(&("agent-a".to_string(), "text-delta".to_string())), Some(&3));
        assert_eq!(snap.get(&("agent-b".to_string(), "finish".to_string())), Some(&1));
    }

    #[test]
    fn redrive_events_counter_accumulates() {
        let reg = MetricsRegistry::new();
        assert!(reg.snapshot_redrive_events().is_empty());
        reg.record_redrive_event("Opex", "cron_redrive_started");
        reg.record_redrive_event("Opex", "cron_redrive_started");
        reg.record_redrive_event("Alma", "interactive_goal_notified");
        let snap = reg.snapshot_redrive_events();
        assert_eq!(
            snap.get(&("Opex".to_string(), "cron_redrive_started".to_string())),
            Some(&2)
        );
        assert_eq!(
            snap.get(&("Alma".to_string(), "interactive_goal_notified".to_string())),
            Some(&1)
        );
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn llm_timeout_counter_increments() {
        let reg = MetricsRegistry::new();
        reg.record_llm_timeout("openai", "inactivity");
        reg.record_llm_timeout("openai", "inactivity");
        reg.record_llm_timeout("openai", "connect");
        reg.record_llm_timeout("anthropic", "request");
        let snap = reg.snapshot_llm_timeout_total();
        assert_eq!(
            snap.get(&("openai".to_string(), "inactivity".to_string())),
            Some(&2)
        );
        assert_eq!(
            snap.get(&("openai".to_string(), "connect".to_string())),
            Some(&1)
        );
        assert_eq!(
            snap.get(&("anthropic".to_string(), "request".to_string())),
            Some(&1)
        );
    }

    #[test]
    fn llm_failover_counter_increments() {
        let reg = MetricsRegistry::new();
        reg.record_llm_failover("primary:openai", "fallback:anthropic", "inactivity");
        reg.record_llm_failover("primary:openai", "fallback:anthropic", "inactivity");
        reg.record_llm_failover("primary:openai", "fallback:google", "5xx");
        let snap = reg.snapshot_llm_failover_total();
        assert_eq!(
            snap.get(&(
                "primary:openai".to_string(),
                "fallback:anthropic".to_string(),
                "inactivity".to_string()
            )),
            Some(&2)
        );
        assert_eq!(
            snap.get(&(
                "primary:openai".to_string(),
                "fallback:google".to_string(),
                "5xx".to_string()
            )),
            Some(&1)
        );
    }

    #[test]
    fn global_metrics_is_installable_and_readable() {
        // Use a fresh registry; do NOT clobber process-wide state across
        // tests — the OnceLock is first-writer-wins, so if another test
        // installed one already, that's fine. We verify `install_global`
        // is callable and `global()` reflects whatever was set (either
        // this one or a prior one — both are valid Arc<MetricsRegistry>s).
        let reg = Arc::new(MetricsRegistry::new());
        install_global(reg.clone());
        assert!(global().is_some(), "global() must return Some after install");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn record_sse_drop_is_thread_safe() {
        let reg = Arc::new(MetricsRegistry::new());
        let mut handles = Vec::new();
        for _ in 0..100 {
            let r = reg.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    r.record_sse_drop("agent-x", "text-delta");
                }
            }));
        }
        for h in handles {
            h.await.expect("task failed");
        }
        let snap = reg.snapshot_sse_drops();
        assert_eq!(
            snap.get(&("agent-x".to_string(), "text-delta".to_string())),
            Some(&10_000)
        );
    }

    #[test]
    fn dashboard_body_emits_cache_token_aggregates() {
        // CACHE-03: four new top-level numeric keys must be present in
        // build_dashboard_body_with_snapshot's output.
        let reg = MetricsRegistry::new();
        let snap = DashboardSnapshot {
            cache_read_tokens_24h: 12_345,
            cache_creation_tokens_24h: 678,
            cache_read_tokens_7d: 99_999,
            cache_creation_tokens_7d: 4_321,
            ..Default::default()
        };
        let body = build_dashboard_body_with_snapshot(&reg, &snap);
        assert_eq!(body["cache_read_tokens_24h"].as_i64(), Some(12_345));
        assert_eq!(body["cache_creation_tokens_24h"].as_i64(), Some(678));
        assert_eq!(body["cache_read_tokens_7d"].as_i64(), Some(99_999));
        assert_eq!(body["cache_creation_tokens_7d"].as_i64(), Some(4_321));
    }

    #[test]
    fn dashboard_body_cache_aggregates_default_to_zero() {
        // Empty snapshot → all four cache fields are 0 (not missing, not null).
        let reg = MetricsRegistry::new();
        let snap = DashboardSnapshot::default();
        let body = build_dashboard_body_with_snapshot(&reg, &snap);
        assert_eq!(body["cache_read_tokens_24h"].as_i64(), Some(0));
        assert_eq!(body["cache_creation_tokens_24h"].as_i64(), Some(0));
        assert_eq!(body["cache_read_tokens_7d"].as_i64(), Some(0));
        assert_eq!(body["cache_creation_tokens_7d"].as_i64(), Some(0));
    }
}
