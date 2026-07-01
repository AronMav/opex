//! File Scenario Engine (FSE) — surviving outcome contract.
//!
//! After the legacy-FSE retirement (2026-07-01) only the toolgate wire type
//! (`ScenarioOutcome`/`ScenarioStatus`, parsed by `gateway/handlers/files.rs`)
//! and its `FSE_DEFAULT_ALLOWLIST` re-export remain here. Dispatch, the enrich
//! seam, the sniffer, the rewrite helper and the owner gate were removed with
//! the legacy post-send chips / Telegram `fse:` path.

pub mod outcome;
// Facade re-export declaring the surviving public surface. The sole live
// consumer (`gateway/handlers/files.rs`) imports via the `outcome` sub-path, so
// the facade alias itself is not imported anywhere — mirror the sibling
// `fse/mod.rs` re-export and suppress the unused-import lint.
#[allow(unused_imports)]
pub use outcome::{FSE_DEFAULT_ALLOWLIST, ScenarioOutcome, ScenarioStatus};
