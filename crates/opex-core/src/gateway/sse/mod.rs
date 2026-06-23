//! Phase 62 RES-01: SSE coalescer module.
//!
//! The engine task emits StreamEvent values into an INNER bounded channel.
//! The coalescer task batches TextDelta events on a 16 ms tick, flushing a
//! single merged TextDelta per window. Non-text events (ToolCallStart,
//! Finish, Error, File, RichCard, etc.) bypass the batch and are forwarded
//! immediately (after flushing any pending text to preserve order).
//!
//! The downstream channel (coalesced_tx) is UNBOUNDED. The coalescer is the
//! sole producer and is rate-limited by the 16 ms tick, so unbounded here
//! is safe — drops therefore happen only when the downstream is closed
//! (receiver gone). MetricsRegistry::record_sse_drop is keyed by
//! (agent, event_type); under normal operation no drops occur.

pub mod coalescer;

pub use coalescer::spawn_coalescing_converter;
