//! Library facade for `hydeclaw-core` integration tests.
//!
//! This crate also has a binary target (`src/main.rs`) — the `[lib]` exists
//! solely so test code can re-export shared types.
//!
//! Phase 61 keeps this surface MINIMAL. The ONLY re-export is `hydeclaw_types`
//! so the integration test mock can construct LlmResponse-shaped values
//! without re-importing the workspace dep at the dev-dep layer.
//!
//! Wave-2 plans (notably Plan 03 db re-export) extend by adding
//! `pub mod` declarations for modules they need — capped at 10 modules total
//! to avoid the lib facade becoming a parallel module tree.
//!
//! DEFERRED (out of scope for Phase 61): exposing `crate::agent::providers::LlmProvider`
//! requires re-including the entire agent subtree (cli_backend, secrets, thinking,
//! providers_*_impl) which exceeds the 10-module cascade cap. Phase 66 REF-01
//! splits engine.rs and naturally exposes a smaller provider surface; the bridge
//! is a one-line blanket impl at that point.

#![allow(clippy::missing_docs_in_private_items)]

// Re-export hydeclaw-types so test code can build LlmResponse values
// without re-importing the workspace dep at the dev-dep layer.
pub use hydeclaw_types;

// ── Phase 62 Plan 02: metrics surface ──────────────────────────────────
// `metrics` is a leaf module with zero crate-internal deps (std + tracing only),
// so re-exporting it here does not cascade the lib facade. Integration tests
// (`integration_dashboard_metrics.rs`) and `tests/support/metrics_probe.rs`
// both reach `MetricsRegistry` via `hydeclaw_core::metrics::MetricsRegistry`.
#[path = "metrics.rs"]
pub mod metrics;

// ── Phase 62 Plan 03: SSE coalescer + StreamEvent leaf exposure ────────
// `agent::stream_event` and `gateway::sse::coalescer` are both leaf modules
// (zero `crate::*` imports) so the lib can expose them for the
// `tests/integration_sse_coalescing.rs` 10k-burst + drop-counter tests.
// We preserve the original paths the binary target uses:
//   * `hydeclaw_core::agent::engine::StreamEvent` — facade that re-exports
//     the leaf enum (same path the binary's `crate::agent::engine::StreamEvent`
//     resolves to). Callers don't need to learn a new path.
//   * `hydeclaw_core::gateway::sse::spawn_coalescing_converter` — leaf
//     coalescer task entry point.
// Neither `agent/engine.rs` nor any other non-leaf module is pulled in.
pub mod agent {
    //! Test-facing re-export subset of the binary's `src/agent/` tree.
    //! ONLY the two leaf modules are exposed — including `engine.rs` would
    //! cascade dozens of `super::*` imports (secrets, providers, tool_loop,
    //! workspace, …) and blow the 10-module lib-facade cap.
    //!
    //! `engine` here is a TINY facade that re-exports `StreamEvent` so
    //! external callers can keep using `agent::engine::StreamEvent`.

    #[path = "stream_event.rs"]
    pub mod stream_event;

    pub mod engine {
        //! Facade preserving `agent::engine::StreamEvent` path.
        pub use super::stream_event::StreamEvent;
    }

    // Phase 64 SEC-02: workspace path canonicalization guard. Leaf module
    // (deps: std + dunce only — zero crate::* references), safe to re-export
    // for integration tests without cascading the agent subtree. Consumed by
    // `tests/integration_path_canonicalize.rs`.
    #[path = "path_guard.rs"]
    pub mod path_guard;
}

// ── Phase 62 Plan 04: shutdown drain surface ───────────────────────────
// `shutdown` is trait-parametric over `DrainableAgent`, so it has zero
// crate-internal deps (only std + tokio + futures-util + tracing). Safe
// to re-export here without cascading the agent subtree into the lib.
// Integration tests (`integration_shutdown_reproducer.rs`) can exercise
// the drain sequence directly against fake handles; the binary target
// wires `AgentHandle: DrainableAgent` in `src/agent/handle.rs`.
#[path = "shutdown.rs"]
pub mod shutdown;

// ── Phase 62 Plan 06: rate limiter sweep() surface ─────────────────────
// `gateway::rate_limiter` is a leaf module (deps: std + tokio::sync::Mutex
// + tracing — no `crate::*` references). We re-export just the leaf via a
// minimal `gateway::middleware` facade so integration tests can reach
// `AuthRateLimiter` / `RequestRateLimiter` at the path they expect:
// `hydeclaw_core::gateway::middleware::{AuthRateLimiter, RequestRateLimiter}`.
// This keeps the test-facing lib surface intact without pulling the gateway
// handler subtree (which would cascade dozens of modules — see Phase 61
// 10-module cap note above).
#[path = "gateway"]
pub mod gateway {
    //! Test-facing re-export subset of the binary's `src/gateway/` tree.
    //! ONLY the leaf `rate_limiter` and `sse` modules are exposed;
    //! `middleware` is a pure re-export facade for the
    //! `middleware::{AuthRateLimiter, ...}` path consumed by Phase 62 RES-04
    //! integration tests. `sse` is exposed for Phase 62 RES-01
    //! integration tests.

    pub use hydeclaw_gateway_util::rate_limiter;

    pub mod middleware {
        //! Facade preserving `gateway::middleware::{AuthRateLimiter, RequestRateLimiter}`
        //! path used by `integration_rate_limiter_sweeper.rs`.
        pub use super::rate_limiter::{AuthRateLimiter, RequestRateLimiter};
    }

    // Phase 62 RES-01: `sse::coalescer` is a leaf module
    // (deps: std + tokio + tracing + `crate::agent::engine::StreamEvent`
    // + `crate::metrics::MetricsRegistry` — both already exposed above).
    // Safe to re-export without cascading the gateway handler subtree.
    #[path = "sse"]
    pub mod sse {
        //! SSE coalescer leaf — safe to re-export for
        //! `integration_sse_coalescing.rs`.

        #[path = "coalescer.rs"]
        pub mod coalescer;

        pub use coalescer::spawn_coalescing_converter;
    }

    // Phase 64 SEC-05: `csp_core` is a leaf module (deps: axum, serde, std,
    // tracing, `crate::metrics::MetricsRegistry` — last one already exposed
    // above). Safe to re-export for `integration_csp_report.rs`. Exposed at
    // path `hydeclaw_core::gateway::csp` so callers don't see the `_core`
    // implementation detail.
    #[path = "csp_core.rs"]
    pub mod csp;

    // Phase 64 SEC-04: `restore_stream_core` is a leaf module (deps: axum,
    // serde_json, futures_util, struson, thiserror, tracing — zero `crate::*`
    // references). Safe to re-export for `integration_backup_size_cap.rs`.
    // Provides `check_content_length_cap`, `drain_body_with_cap`, `CapExceeded`,
    // `parse_stream_value` — the primitives POST /api/restore uses to enforce
    // max_restore_size_mb without loading the whole body.
    pub use hydeclaw_gateway_util::restore_stream_core;

    // Phase 65 OBS-04: `trace_context` is a leaf module (deps: axum, tracing,
    // uuid — zero `crate::*` references). Safe to re-export for
    // `integration_trace_context.rs`. Provides `parse_traceparent`,
    // `new_trace_id`, `TraceId`, `trace_context_middleware` — the primitives
    // for the W3C Trace Context middleware that sits upstream of
    // `auth_middleware` in the router chain.
    //
    // Exposed inside the existing `gateway` facade (not a new top-level
    // `pub mod`), so the 10-module lib-facade cap stays at 7 top-level mods.
    pub use hydeclaw_gateway_util::trace_context;
}

// ── DB modules extracted to hydeclaw-db ────────────────────────────────
// Re-exported here so integration tests can reach them at the same
// `hydeclaw_core::db::*` paths they already use. New DB modules go into
// hydeclaw-db directly — no new entries needed here.
pub mod db {
    pub use hydeclaw_db::approvals;
    pub use hydeclaw_db::notifications;
    pub use hydeclaw_db::session_wal;
    pub use hydeclaw_db::sessions;
    pub use hydeclaw_db::usage;
}

// ── Phase 64 SEC-01: unified SSRF guard ────────────────────────────────
// `net::ssrf` is a leaf module (deps: std + reqwest::dns + tokio::net +
// thiserror + url). No `crate::*` references, so re-exposing it here does
// NOT cascade any other subtree into the lib facade.
//
// Consumed by:
//   * tests/integration_ssrf_guard.rs  (DNS-rebinding + expanded IP set)
//   * tests/integration_webhook_ssrf.rs (shared-guard contract for future
//     webhook outbound delivery code paths — see 64-02-SUMMARY.md for the
//     no-existing-client deviation note).
#[path = "net"]
pub mod net {
    //! Test-facing re-export subset of the binary's `src/net/` tree.
    //! Only `ssrf` is exposed today — any future `net::*` leaf added to
    //! the binary must be opted in here explicitly.

    #[path = "ssrf.rs"]
    pub mod ssrf;
}

// ── Phase 64 SEC-03: signed upload URL mint/verify ─────────────────────
// Leaf module (deps: std + base64 + hmac + sha2 + hkdf + subtle + thiserror —
// zero crate::* references). Safe to re-export without cascading the lib
// surface. Consumed by `tests/integration_upload_hmac.rs`.
//
// Top-level `pub mod` accounting (per src/lib.rs 10-module cap):
//   metrics, agent, shutdown, gateway, db, net, uploads = 7. OK.
#[path = "uploads.rs"]
pub mod uploads;

// ── ts-gen codegen surface ─────────────────────────────────────────────
// Exposes DTO types for the `gen_ts_types` binary (feature-gated so
// production builds never pull in ts-rs). All included modules are leaf
// modules with zero crate-internal imports — safe to include here without
// cascading config/memory/etc.
#[cfg(feature = "ts-gen")]
pub mod dto_export;
