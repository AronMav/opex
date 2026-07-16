//! Registration of the global UI WebSocket event types for ts-rs codegen.
//!
//! These types live in `opex-types`; this module re-imports them and
//! registers via `register_ts_dto!(.., dest = "ui-ws")` so they emit to
//! `ui/src/types/ws.generated.ts` instead of api.generated.ts.
//!
//! Wire format invariant: WsEvent is a Serde-tagged enum
//! (`#[serde(tag = "type")]`); ts-rs preserves the same shape.
//!
//! See `.superpowers/sdd/task-7-brief.md` / `task-7-report.md`.

#[allow(unused_imports)]
use opex_types::ws::{NotificationReadData, NotificationsReadAllData, WsEvent};

crate::register_ts_dto!(WsEvent,                  dest = "ui-ws");
crate::register_ts_dto!(NotificationReadData,     dest = "ui-ws");
crate::register_ts_dto!(NotificationsReadAllData, dest = "ui-ws");
