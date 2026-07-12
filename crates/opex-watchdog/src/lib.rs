//! Library re-exports for integration tests.
//!
//! The watchdog is primarily a binary (`src/main.rs`). This thin library
//! crate exposes the internal modules so `tests/` can drive them (e.g.
//! the inactivity integration test that mocks both the core endpoint
//! and the channel-notify endpoint via wiremock).

pub mod alerter;
pub mod config;
pub mod inactivity;
pub mod infra_jobs;
pub mod infra_watch;
