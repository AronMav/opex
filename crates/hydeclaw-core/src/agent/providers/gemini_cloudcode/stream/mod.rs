//! Streaming layer for GeminiCloudCode: SSE parsing + delta synthesis.
//!
//! # Sub-modules
//! - [`sse_parser`] — splits raw SSE bytes into typed [`GeminiStreamEvent`]s
//! - [`delta`] — translates events into provider-neutral [`DeltaChunk`]s
#![allow(dead_code, unused_imports)]

pub(super) mod sse_parser;
pub(super) mod delta;

pub(super) use sse_parser::{
    GeminiStreamEvent, GeminiCandidate, GeminiContent, GeminiPart,
    GeminiUsageMetadata, parse_sse_events,
};
pub(super) use delta::{DeltaChunk, events_to_deltas};
