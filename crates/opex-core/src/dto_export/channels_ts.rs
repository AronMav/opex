//! Registration of channel WS protocol types for ts-rs codegen.
//!
//! These types live in `opex-types`; this module re-imports them and
//! registers via `register_ts_dto!(.., dest = "channels")` so they emit
//! to `channels/src/types.generated.ts` instead of the UI surface.
//!
//! Wire format invariant: ChannelInbound/Outbound are Serde-tagged enums
//! (`#[serde(tag = "type")]`); ts-rs preserves the same shape.
//!
//! See docs/superpowers/specs/2026-05-07-s6-channels-codegen-design.md.

// Types are referenced inside `register_ts_dto!()` macro expansions
// (`<$t as ::ts_rs::TS>::decl(...)`). Verified clippy-clean without
// `#[allow(unused_imports)]` after T3 — rustc tracks the usage correctly.
use opex_types::{
    ChannelActionDto, ChannelInbound, ChannelOutbound,
    IncomingMessageDto, MediaAttachment, MediaType,
};

crate::register_ts_dto!(MediaType,            dest = "channels");
crate::register_ts_dto!(MediaAttachment,      dest = "channels");
crate::register_ts_dto!(IncomingMessageDto,   dest = "channels");
crate::register_ts_dto!(ChannelActionDto,     dest = "channels");
crate::register_ts_dto!(ChannelInbound,       dest = "channels");
crate::register_ts_dto!(ChannelOutbound,      dest = "channels");
