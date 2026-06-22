//! File Scenario Engine (FSE) — core dispatch layer.
//!
//! Phase 2 scope: the core-owned outcome contract (`outcome`) and the in-core
//! built-in action dispatch table (`dispatch`). Wiring into
//! `enrich_message_text` (Phase 3), the sniffer (Phase 1), and the bindings
//! table / HTTP routes (later phases) are NOT here.

pub mod dispatch;
pub mod dispatch_seam;
pub mod outcome;
pub mod rewrite;
pub mod sniff;

// `ScenarioOutcome` and `ScenarioStatus` are used directly via sub-module paths
// in Phase 3 code; the remaining re-exports (`resolve`, `BuiltinAction`,
// `dispatch_attachments`, `ScenarioChoice`, `FSE_DEFAULT_ALLOWLIST`) become
// the public API consumed by Phase 4+ HTTP routes and Phase 6 emission.
// Until those phases land, the re-exports are pub API only — keep them but
// suppress the unused_imports lint that fires because no caller imports via
// this module facade yet.
#[allow(unused_imports)] // Phase 4+: consumed by HTTP route handlers
pub use dispatch::{resolve, BuiltinAction};
#[allow(unused_imports)] // Phase 6: ScenarioChoice/dispatch_attachments consumed by affordance emitter
pub use dispatch_seam::{dispatch_attachments, PendingAlternative, ScenarioChoice};
#[allow(unused_imports)] // Phase 4+: FSE_DEFAULT_ALLOWLIST consumed by binding validator; ScenarioOutcome/ScenarioStatus used via sub-paths
pub use outcome::{FSE_DEFAULT_ALLOWLIST, ScenarioOutcome, ScenarioStatus};
