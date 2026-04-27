//! Re-export surface for `gen_ts_types`. Gated behind `ts-gen`.
//!
//! Rules for adding entries here:
//! 1. Only leaf modules (no `crate::*` imports) — prevents lib-facade cascade.
//! 2. Always-on modules (like `db::approvals`) can be re-exported via `pub use`.
//! 3. Modules not already in lib.rs need a `#[path]` entry here (ts-gen only).
//!
//! `#[path]` attributes resolve relative to this file's directory (src/dto_export/),
//! so `../gateway/...` navigates into the sibling gateway/ directory under src/.

/// Distributed TypeScript export registry — used by gen_ts_types.
///
/// Always-on (no ts-rs deps in the non-ts-gen branch) so that DTO call sites
/// across the crate can invoke `crate::register_ts_dto!(...)` unconditionally.
pub mod registry;

// Everything below is ts-gen only — pulls ts-rs into the codegen surface.

/// Phase B: AgentDetail DTO tree (12 structs).
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/agents/dto_structs.rs"]
pub mod agents_dto;

/// Phase C: GitHubRepo — leaf module (anyhow, sqlx, uuid, chrono; no crate::*).
#[cfg(feature = "ts-gen")]
#[path = "../db/github.rs"]
pub mod github_dto;

/// Phase C: AllowlistEntry — already in lib's always-on db::approvals surface.
/// Re-exported here so gen_ts_types can import from one predictable place.
#[cfg(feature = "ts-gen")]
pub use crate::db::approvals::AllowlistEntry;
#[cfg(feature = "ts-gen")]
crate::register_ts_dto!(AllowlistEntry);

/// Phase A W1: DB notification types — already in always-on db::notifications.
#[cfg(feature = "ts-gen")]
pub use crate::db::notifications::{Notification, NotificationsResponseDto};
#[cfg(feature = "ts-gen")]
crate::register_ts_dto!(Notification);
#[cfg(feature = "ts-gen")]
crate::register_ts_dto!(NotificationsResponseDto);

/// Phase A W1: DB session + message types — already in always-on db::sessions.
#[cfg(feature = "ts-gen")]
pub use crate::db::sessions::{Session, MessageRow};
#[cfg(feature = "ts-gen")]
crate::register_ts_dto!(Session);
#[cfg(feature = "ts-gen")]
crate::register_ts_dto!(MessageRow);

/// Phase A W2: Channel row + active channel DTOs — leaf file, no crate::* imports.
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/channels_dto_structs.rs"]
pub mod channels_dto;

/// Phase A W2: Cron job + run DTOs — leaf file, no crate::* imports.
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/cron_dto_structs.rs"]
pub mod cron_dto;

/// Phase A W2: Memory document + stats DTOs — leaf file, no crate::* imports.
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/memory_dto_structs.rs"]
pub mod memory_dto;

/// Phase A W3: Tool service + MCP DTOs
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/tools_dto_structs.rs"]
pub mod tools_dto;

/// Phase A W3: Webhook list DTO
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/webhooks_dto_structs.rs"]
pub mod webhooks_dto;

/// Phase A W3: Approval list DTO
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/agents/approvals_dto_structs.rs"]
pub mod approvals_dto;

/// Backup file list DTO. The same source file is also `#[path]`-included by
/// `gateway::handlers::backup` so the struct shape is single-sourced. The
/// `register_ts_dto!()` call lives here to avoid double-registration (the file
/// is compiled twice, once per `#[path]` host).
#[cfg(feature = "ts-gen")]
#[path = "../gateway/handlers/backup_dto_structs.rs"]
pub mod backup_dto;

#[cfg(feature = "ts-gen")]
mod backup_dto_register {
    // `register_ts_dto!()` uses `stringify!($t)` for the name field, so $t must
    // resolve as a bare identifier (`BackupEntryDto`) — not a path. Bring it
    // into a private module scope first, then register.
    use super::backup_dto::BackupEntryDto;
    crate::register_ts_dto!(BackupEntryDto);
}

