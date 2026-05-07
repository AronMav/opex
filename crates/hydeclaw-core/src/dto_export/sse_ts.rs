//! Registration of SSE wire-protocol types for ts-rs codegen.
//!
//! These types live in `hydeclaw-types`; this module re-imports them and
//! registers via `register_ts_dto!(.., dest = "ui-sse")` so they emit
//! to `ui/src/types/sse.generated.ts` instead of api.generated.ts.
//!
//! Wire format invariant: SseEvent is a Serde-tagged enum
//! (`#[serde(tag = "type")]`); ts-rs preserves the same shape.
//!
//! See docs/superpowers/specs/2026-05-07-s6.5-sse-codegen-design.md.

#[allow(unused_imports)]
use hydeclaw_types::sse::{
    DataSessionIdPayload, MetricCard, MetricTrend, RichCardData, SseEvent,
    SyncStatus, TableCard, UsagePayload,
};

crate::register_ts_dto!(SseEvent,             dest = "ui-sse");
crate::register_ts_dto!(DataSessionIdPayload, dest = "ui-sse");
crate::register_ts_dto!(RichCardData,         dest = "ui-sse");
crate::register_ts_dto!(TableCard,            dest = "ui-sse");
crate::register_ts_dto!(MetricCard,           dest = "ui-sse");
crate::register_ts_dto!(MetricTrend,          dest = "ui-sse");
crate::register_ts_dto!(SyncStatus,           dest = "ui-sse");
crate::register_ts_dto!(UsagePayload,         dest = "ui-sse");
