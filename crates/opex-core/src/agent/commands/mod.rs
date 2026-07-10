//! Единый реестр команд чата (спек 2026-07-09).
//!
//! Phase 1 exposed a static `COMMAND_REGISTRY` `LazyLock` (builtins only).
//! Phase 2a's `merge::build_registry` rebuilds the merged registry
//! (builtin + live handler commands) per request instead — the live
//! `HandlerRegistry` manifest set is hot-reloaded, so a one-shot `LazyLock`
//! of builtins-only can't represent it. The singleton was removed here.

pub mod spec;
pub mod registry;
pub mod builtin;
pub mod handler_source;
pub mod merge;
pub mod dispatch;
