//! Tool dispatcher: meta-tool indirection that lets the LLM discover and
//! invoke "extension" tools (YAML, MCP, rare system) via `tool_use(...)`
//! without having their full schemas in the per-turn `tools` array.
//!
//! See: docs/superpowers/specs/2026-05-06-tool-dispatcher-design.md

pub mod lookup;
pub mod state;

// allow(unused_imports): re-exports are consumed by Task 7 onward
// (tool_handlers/tool_use.rs, engine/context_builder.rs); landed early
// alongside the helper definitions.
#[allow(unused_imports)]
pub use lookup::{build_extension_tool_list, find_extension_tool, is_known_tool, is_valid_tool_name};
#[allow(unused_imports)]
pub use state::{SessionToolState, SessionToolStateMap};
