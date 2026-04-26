//! Re-export surface for `gen_ts_types`. Gated behind `ts-gen`.
//!
//! Rules for adding entries here:
//! 1. Only leaf modules (no `crate::*` imports) — prevents lib-facade cascade.
//! 2. Always-on modules (like `db::approvals`) can be re-exported via `pub use`.
//! 3. Modules not already in lib.rs need a `#[path]` entry here (ts-gen only).
//!
//! `#[path]` attributes resolve relative to this file's directory (src/dto_export/),
//! so `../gateway/...` navigates into the sibling gateway/ directory under src/.

/// Phase B: AgentDetail DTO tree (12 structs).
#[path = "../gateway/handlers/agents/dto_structs.rs"]
pub mod agents_dto;

/// Phase C: GitHubRepo — leaf module (anyhow, sqlx, uuid, chrono; no crate::*).
#[path = "../db/github.rs"]
pub mod github_dto;

/// Phase C: AllowlistEntry — already in lib's always-on db::approvals surface.
/// Re-exported here so gen_ts_types can import from one predictable place.
pub use crate::db::approvals::AllowlistEntry;

/// Phase A W1: DB notification types — already in always-on db::notifications.
pub use crate::db::notifications::{Notification, NotificationsResponseDto};

/// Phase A W1: DB session + message types — already in always-on db::sessions.
pub use crate::db::sessions::{Session, MessageRow};

/// Phase A W2: Channel row + active channel DTOs — leaf file, no crate::* imports.
#[path = "../gateway/handlers/channels_dto_structs.rs"]
pub mod channels_dto;

/// Phase A W2: Cron job + run DTOs — leaf file, no crate::* imports.
#[path = "../gateway/handlers/cron_dto_structs.rs"]
pub mod cron_dto;

/// Phase A W2: Memory document + stats DTOs — leaf file, no crate::* imports.
#[path = "../gateway/handlers/memory_dto_structs.rs"]
pub mod memory_dto;

/// Phase A W3: Tool service + MCP DTOs
#[path = "../gateway/handlers/tools_dto_structs.rs"]
pub mod tools_dto;

/// Phase A W3: Webhook list DTO
#[path = "../gateway/handlers/webhooks_dto_structs.rs"]
pub mod webhooks_dto;

/// Phase A W3: Approval list DTO
#[path = "../gateway/handlers/agents/approvals_dto_structs.rs"]
pub mod approvals_dto;

/// Backup file list DTO
#[path = "../gateway/handlers/backup_dto_structs.rs"]
pub mod backup_dto;

