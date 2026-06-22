//! File Scenario Engine (FSE) — core dispatch layer.
//!
//! Phase 2 scope: the core-owned outcome contract (`outcome`) and the in-core
//! built-in action dispatch table (`dispatch`). Wiring into
//! `enrich_message_text` (Phase 3), the sniffer (Phase 1), and the bindings
//! table / HTTP routes (later phases) are NOT here.

pub mod outcome;

pub use outcome::{ScenarioOutcome, ScenarioStatus};
