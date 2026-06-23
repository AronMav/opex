//! Shared test harness for HydeClaw integration tests.
//!
//! Each test binary under `tests/` opts in via `mod support;` and uses the re-exports below.
//! Wave-2 plans (approval_race, sse_lifecycle, shutdown_drain) build on this module.

#![allow(dead_code)] // Each integration binary uses a different subset; silence unused warnings.
#![allow(unused_imports)] // Re-exports not used by every test binary.

pub mod drain_fixture;
pub mod fixtures;
pub mod harness;
pub mod metrics_probe;
pub mod migrations;
pub mod mock_provider;
pub mod sse_recorder;
pub mod toolgate_fixture;

// Phase 64 fixtures (Wave 0 scaffolding for Plans 02–06).
pub mod backup_fixture;
pub mod dns_fixture;
pub mod signed_url_helper;

pub use drain_fixture::DrainFixture;
pub use harness::TestHarness;
pub use metrics_probe::{BoundMetricsProbe, MetricsProbe};
pub use mock_provider::{MockLlmProvider, MockProvider, MockTurn};
pub use sse_recorder::{SseRecorder, TestStreamEvent};
pub use toolgate_fixture::{SpawnResult, ToolgateFixture};

// Phase 64 re-exports.
pub use backup_fixture::synthesize_backup_bytes;
pub use dns_fixture::DnsRebindingResolver;
pub use signed_url_helper::{mint_signed_url, parse_signed_url, tampered_sig, ParsedSignedUrl};
