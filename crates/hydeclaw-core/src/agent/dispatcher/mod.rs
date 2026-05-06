//! Tool dispatcher: meta-tool indirection that lets the LLM discover and
//! invoke "extension" tools (YAML, MCP, rare system) via `tool_use(...)`
//! without having their full schemas in the per-turn `tools` array.
//!
//! See: docs/superpowers/specs/2026-05-06-tool-dispatcher-design.md

pub mod state;

pub use state::{SessionToolState, SessionToolStateMap};
