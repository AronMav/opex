//! Library facade for `opex-core` integration tests.
//!
//! This crate also has a binary target (`src/main.rs`) — the `[lib]` exists
//! so test code in `tests/*.rs` can reach a curated subset of the binary's
//! module tree. Each `pub mod` here `#[path]`-mounts a leaf module from
//! `src/`; binary and lib share the same source files, so changes land in
//! one place.
//!
//! Discipline: only re-export **leaf** modules (zero `crate::*` deps).
//! Pulling in non-leaf modules cascades dependencies and grows the lib
//! tree into a parallel binary tree. When a test needs production code
//! that isn't a leaf, the right pattern is to add the test inline next to
//! the production code as `#[cfg(test)] mod tests { … }` (see
//! `agent/pipeline/behaviour.rs::tests` and `memory/store.rs::search_hybrid_rrf_tests`)
//! rather than expanding the lib facade.

#![allow(clippy::missing_docs_in_private_items)]

// Re-export opex-types so test code can build LlmResponse values
// without re-importing the workspace dep at the dev-dep layer.
pub use opex_types;

// `metrics` and `trace_propagation` are both leaf modules — std + tracing /
// std + reqwest + opentelemetry, no `crate::*` references. Consumed by
// `tests/integration_dashboard_metrics.rs` and the cross-process tracing
// integration tests respectively.
#[path = "metrics.rs"]
pub mod metrics;

#[path = "trace_propagation.rs"]
pub mod trace_propagation;

// `agent::stream_event` (the `StreamEvent` enum) and `agent::path_guard`
// are leaves consumed by `integration_sse_coalescing.rs` and
// `integration_path_canonicalize.rs`. The `engine` sub-namespace is a
// pure re-export so callers using `crate::agent::engine::StreamEvent`
// (the binary path) keep working through the lib too.
//
// `agent::file_scenario::outcome` and `agent::url_tools` are exposed for the
// surviving FSE integration coverage. All mounted modules are near-leaf:
// - `fse/allowlist.rs` — pure data + serde + fmt (no crate::* deps)
// - `url_tools.rs`    — opex_types only (leaf)
// - `outcome.rs`      — serde + fse::allowlist (both in this lib tree)
pub mod agent {
    #[path = "stream_event.rs"]
    pub mod stream_event;

    pub mod engine {
        pub use super::stream_event::StreamEvent;
    }

    #[path = "path_guard.rs"]
    pub mod path_guard;

    // `url_tools.rs` is a near-leaf (opex_types only) exposed to integration
    // tests. `enrich_with_attachments` and `extract_urls` are `pub(crate)` and
    // used by the binary only, so the dead_code lint fires in the lib context
    // (they appear unreachable from this lib tree). Suppress it: they are not
    // dead in the binary crate.
    //
    // NOTE: `#[path]` inside `pub mod agent { }` resolves relative to
    // `src/agent/` (the implicit directory for the `agent` inline module),
    // matching Rust's normal module file search rules.
    #[path = "url_tools.rs"]
    #[allow(dead_code)]
    pub mod url_tools;

    pub mod fse {
        // Pure-data constants + validators. No crate::* deps.
        // Path resolves relative to `src/agent/fse/`.
        #[path = "allowlist.rs"]
        pub mod allowlist;

        // allowlist_store — DB-backed toggle storage. Needed by url_tools'
        // enrich_with_attachments to resolve the enabled-handler allowlist.
        #[path = "allowlist_store.rs"]
        #[allow(dead_code)]
        pub mod allowlist_store;

        pub use allowlist_store::get_enabled_allowlist;
    }

    // handler_registry — toolgate handler manifest cache + match_buttons.
    // Needed by url_tools' enrich_with_attachments to resolve available
    // handlers for a given file attachment.
    #[path = "handler_registry.rs"]
    #[allow(dead_code)]
    pub mod handler_registry;

    pub mod file_scenario {
        // Surviving toolgate wire type parsed by `gateway/handlers/files.rs`.
        #[path = "outcome.rs"]
        pub mod outcome;
        pub use outcome::{ScenarioOutcome, ScenarioStatus};
    }
}

// `shutdown` is trait-parametric (only std + tokio + futures-util + tracing).
// `tests/integration_shutdown_reproducer.rs` exercises the drain sequence
// against fake handles; the binary wires `AgentHandle: DrainableAgent` in
// `src/agent/handle.rs`.
#[path = "shutdown.rs"]
pub mod shutdown;

// `gateway` re-exports leaves from `opex-gateway-util` plus two
// crate-local leaves (`sse::coalescer`, `csp`). All consumed by
// `tests/integration_*.rs`. New `gateway::*` lib entries should live in
// `opex-gateway-util` rather than under `src/gateway/handlers/` so
// the lib facade doesn't drag the handler subtree.
#[path = "gateway"]
pub mod gateway {
    pub use opex_gateway_util::rate_limiter;
    pub use opex_gateway_util::restore_stream_core;
    pub use opex_gateway_util::trace_context;

    pub mod middleware {
        // Facade preserving `gateway::middleware::{AuthRateLimiter, RequestRateLimiter, valid_bearer}`.
        pub use super::rate_limiter::{AuthRateLimiter, RequestRateLimiter, valid_bearer};
    }

    #[path = "sse"]
    pub mod sse {
        #[path = "coalescer.rs"]
        pub mod coalescer;
        pub use coalescer::spawn_coalescing_converter;
    }

    // Exposed at path `gateway::csp` so callers don't see the `_core`
    // implementation-detail filename.
    #[path = "csp_core.rs"]
    pub mod csp;
}

// `db` re-exports leaves from `opex-db` plus three crate-local DB
// leaves (`curator_runs`, `access`, `upload_migration`). New DB modules
// should live in `opex-db` so this list shrinks over time.
pub mod db {
    pub use opex_db::approvals;
    pub use opex_db::memory_queries;
    pub use opex_db::notifications;
    pub use opex_db::session_timeline;
    pub use opex_db::sessions;
    pub use opex_db::usage;

    // `#[path]` inside an inline module block resolves relative to the
    // *parent file's directory* — i.e., `src/db/` here.
    #[path = "curator_runs.rs"]
    pub mod curator_runs;

    // Needed so the `channels::access` re-mount below can resolve
    // `crate::db::access` from inside `channels/access.rs`.
    #[path = "access.rs"]
    pub mod access;

    #[path = "upload_migration.rs"]
    pub mod upload_migration;

    #[path = "uploads.rs"]
    pub mod uploads;
}

// `channels::access` exposes `AccessGuard` for
// `tests/integration_approval_security.rs`. The full channel-manager
// subtree has many `crate::*` deps and is intentionally NOT re-exported.
pub mod channels {
    #[path = "access.rs"]
    pub mod access;
}

// `net::ssrf` is the unified SSRF guard (Phase 64 SEC-01). Leaf, no
// `crate::*` deps. Consumed by `integration_ssrf_guard.rs` and
// `integration_webhook_ssrf.rs`.
#[path = "net"]
pub mod net {
    #[path = "ssrf.rs"]
    pub mod ssrf;
}

// `uploads` is the signed upload URL mint/verify primitive (Phase 64
// SEC-03). Leaf, no `crate::*` deps. Consumed by `integration_upload_hmac.rs`.
#[path = "uploads.rs"]
pub mod uploads;

// `dto_export` is always-on (regardless of feature flags) because the
// `register_ts_dto!` macro is reachable from DTO call sites and acts as a
// no-op when `ts-gen` is OFF. All ts-rs-dependent re-exports inside
// `dto_export/mod.rs` are individually gated behind `#[cfg(feature = "ts-gen")]`,
// so production builds still don't pull in ts-rs.
pub mod dto_export;

// ── Memory module: NOT exposed from the lib facade. ───────────────────
// The `memory_test_facade` lib-bridge that used to live here was deleted
// during the lib.rs facade cleanup (its sole consumer,
// `tests/test_search_hybrid_rrf.rs`, was moved inline to `memory/store.rs`
// as `#[cfg(test)] mod search_hybrid_rrf_tests`). If a future test needs
// `MemoryStore` from a separate `tests/*.rs` file, the right move is to
// add the test inline next to the production code rather than reviving
// the bridge.
