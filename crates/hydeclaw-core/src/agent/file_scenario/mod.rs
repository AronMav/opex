//! File Scenario Engine (FSE) — core dispatch layer.
//!
//! Phase 2 scope: the core-owned outcome contract (`outcome`) and the in-core
//! built-in action dispatch table (`dispatch`). Wiring into
//! `enrich_message_text` (Phase 3), the sniffer (Phase 1), and the bindings
//! table / HTTP routes (later phases) are NOT here.

// The dispatcher's public surface is consumed starting in Phase 3
// (wired into `enrich_message_text`). Until then the items are reachable only
// from tests, so `dead_code` and `unused_imports` (re-exports) would fire.
// Remove this allow once Phase 3 lands.
#![allow(dead_code, unused_imports)]

pub mod dispatch;
pub mod outcome;
pub mod sniff;

pub use dispatch::{resolve, BuiltinAction};
pub use outcome::{FSE_DEFAULT_ALLOWLIST, ScenarioOutcome, ScenarioStatus};
