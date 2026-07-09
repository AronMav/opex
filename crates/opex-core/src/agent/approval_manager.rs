#![deny(clippy::await_holding_lock)]
//! Approval workflow manager: check/create/wait/cleanup for tool-call approvals.
//!
//! The pending-waiter map is backed by `DashMap` (sharded, synchronous
//! lock-per-bucket), avoiding the "hold write guard across `.await`" anti-pattern.
//! `#![deny(clippy::await_holding_lock)]` ensures this cannot regress.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use opex_types::approvals::ApprovalAction;
use opex_types::ids::ApprovalId;
use sqlx::PgPool;
use uuid::Uuid;

use super::channel_actions::{ChannelAction, ChannelActionRouter};
use super::engine::{ApprovalResult, StreamEvent};

// ── Types ────────────────────────────────────────────────────────────────────

/// Map of pending approval waiters: approval_id → (oneshot sender, creation time).
///
/// `DashMap` shards internally — each bucket is protected by its own sync
/// `RwLock`. Guards returned by `get()` / `get_mut()` are RAII and MUST NOT be
/// held across `.await`; the module-level `await_holding_lock` deny lint
/// enforces this at compile time.
///
/// T4: key was `Uuid`, now `ApprovalId` — wire format unchanged because the
/// newtype is `#[serde(transparent)]`. Hash/Eq/Copy come through `impl_id_newtype!`.
pub(crate) type ApprovalWaitersMap =
    Arc<DashMap<ApprovalId, (tokio::sync::oneshot::Sender<ApprovalResult>, Instant)>>;

/// Outcome of `request_approval`: tells the caller how to proceed.
#[derive(Debug)]
pub(crate) enum ApprovalOutcome {
    /// Tool was approved — execute with original arguments.
    Approved,
    /// Tool was approved with modified arguments — caller should re-dispatch.
    ApprovedWithModifiedArgs(serde_json::Value),
    /// Tool was rejected by the user.
    Rejected(String),
    /// Approval was cancelled (sender dropped).
    Cancelled,
    /// Approval timed out.
    TimedOut { timeout_secs: u64 },
}

// ── ApprovalManager ──────────────────────────────────────────────────────────

/// Encapsulates the full approval lifecycle: DB record, channel notification,
/// UI broadcast, waiter management, and timeout handling.
pub(crate) struct ApprovalManager {
    db: PgPool,
    waiters: ApprovalWaitersMap,
}

impl ApprovalManager {
    pub(crate) fn new(db: PgPool, waiters: ApprovalWaitersMap) -> Self {
        Self { db, waiters }
    }

    /// Shared waiters map — needed by `resolve_approval` on `AgentEngine`.
    pub(crate) fn waiters(&self) -> &ApprovalWaitersMap {
        &self.waiters
    }

    /// Request approval for a tool call. Blocks until approved, rejected, or timed out.
    ///
    /// Steps:
    /// 1. Create DB approval record
    /// 2. Audit + broadcast UI event
    /// 3. Send approval request via channel router (if available)
    /// 4. Emit SSE `ApprovalNeeded` event
    /// 5. Wait with timeout
    /// 6. Clean up waiter on completion/timeout/error
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request_approval(
        &self,
        agent_name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        context: &serde_json::Value,
        timeout_secs: u64,
        channel_router: Option<&ChannelActionRouter>,
        ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
        sse_event_tx: &Arc<dashmap::DashMap<Uuid, crate::agent::engine_event_sender::EngineEventSender>>,
    ) -> ApprovalOutcome {
        let session_id = context
            .get("session_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        // 1. Create DB record
        let approval_id = match crate::db::approvals::create_approval(
            &self.db,
            agent_name,
            session_id,
            tool_name,
            arguments,
            context,
        )
        .await
        {
            Ok(id) => {
                crate::db::audit::audit_spawn(
                    self.db.clone(),
                    agent_name.to_string(),
                    crate::db::audit::event_types::APPROVAL_REQUESTED,
                    None,
                    serde_json::json!({
                        "tool": tool_name, "approval_id": id.to_string()
                    }),
                );
                Self::broadcast_ui(ui_event_tx, serde_json::json!({
                    "type": "approval_requested",
                    "approval_id": id.to_string(),
                    "agent": agent_name,
                    "tool_name": tool_name,
                }));
                // Fire-and-forget notification
                if let Some(ui_tx) = ui_event_tx {
                    let db = self.db.clone();
                    let tx = ui_tx.clone();
                    let tool_name_owned = tool_name.to_string();
                    let agent_name_owned = agent_name.to_string();
                    let approval_id_str = id.to_string();
                    // AUDIT-FF-001: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
                    tokio::spawn(async move {
                        crate::gateway::notify(
                            &db,
                            &tx,
                            "tool_approval",
                            "Tool Approval Required",
                            &format!(
                                "Agent {} is requesting approval to use tool: {}",
                                agent_name_owned, tool_name_owned
                            ),
                            serde_json::json!({
                                "agent": agent_name_owned,
                                "tool_name": tool_name_owned,
                                "approval_id": approval_id_str,
                            }),
                        )
                        .await
                        .ok();
                    });
                }
                id
            }
            Err(e) => return ApprovalOutcome::Rejected(format!("Error creating approval: {}", e)),
        };

        // 2. Send approval request via channel adapter
        let clean_args = {
            let mut args_clone = arguments.clone();
            if let Some(obj) = args_clone.as_object_mut() {
                obj.remove("_context");
            }
            args_clone
        };

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let action = ChannelAction {
            name: "approval_request".to_string(),
            params: serde_json::json!({
                "tool_name": tool_name,
                "args": clean_args,
                "approval_id": approval_id.to_string(),
            }),
            context: context.clone(),
            reply: reply_tx,
            target_channel: None,
        };
        if let Some(router) = channel_router {
            if let Err(e) = router.send(action).await {
                tracing::error!(
                    approval_id = %approval_id,
                    error = %e,
                    "failed to send approval_request to channel"
                );
            }
            tokio::time::timeout(Duration::from_secs(5), reply_rx)
                .await
                .ok();
        } else {
            tracing::warn!(
                tool = %tool_name,
                "no channel_router — cannot send approval buttons"
            );
        }

        // 3. Create oneshot waiter and insert into map.
        //    DashMap is sync and sharded — no cross-await lock held.
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        // F035: no hardcoded 300s eviction here either — it force-cancelled
        // still-pending approvals of the SAME agent (shared per-agent map) that
        // had a longer configured timeout. Each waiter enforces its own
        // deadline below; prune_stale reaps orphans.
        self.waiters.insert(approval_id, (result_tx, Instant::now()));

        // 4. Emit SSE event for inline approval in chat UI. F036: target the
        //    sender for THIS approval's session only — ApprovalNeeded carries
        //    tool_name + tool_input, so mis-delivery to a concurrent session of
        //    the same agent would leak it. Clone the sender out of the map
        //    BEFORE the await (never hold a DashMap Ref across await).
        let approval_sender =
            session_id.and_then(|sid| sse_event_tx.get(&sid).map(|r| r.clone()));
        if let Some(tx) = approval_sender {
            let clean_input = {
                let mut args_clone = arguments.clone();
                if let Some(obj) = args_clone.as_object_mut() {
                    obj.remove("_context");
                }
                args_clone
            };
            // ApprovalNeeded is a non-text event that MUST be delivered —
            // losing it would strand the client waiting indefinitely.
            // send_async blocks until a slot is available (or closed), honoring
            // the EngineEventSender "non-text never dropped" contract.
            if let Err(e) = tx
                .send_async(StreamEvent::ApprovalNeeded {
                    approval_id,
                    tool_name: tool_name.to_string(),
                    tool_input: clean_input,
                    timeout_ms: timeout_secs * 1000,
                })
                .await
            {
                tracing::warn!(approval_id = %approval_id, error = ?e, "ApprovalNeeded send failed");
            }
        }

        // 5. Wait for approval with timeout
        match tokio::time::timeout(Duration::from_secs(timeout_secs), result_rx).await {
            Ok(Ok(ApprovalResult::Approved)) => {
                tracing::info!(tool = %tool_name, approval_id = %approval_id, "tool approved");
                ApprovalOutcome::Approved
            }
            Ok(Ok(ApprovalResult::ApprovedWithModifiedArgs(modified_args))) => {
                tracing::info!(
                    tool = %tool_name,
                    approval_id = %approval_id,
                    "tool approved with modified args"
                );
                ApprovalOutcome::ApprovedWithModifiedArgs(modified_args)
            }
            Ok(Ok(ApprovalResult::Rejected(reason))) => {
                ApprovalOutcome::Rejected(format!("Tool `{}` was rejected: {}", tool_name, reason))
            }
            Ok(Err(_)) => {
                // Sender dropped (pruned or retain cleanup) — resolve DB record.
                self.waiters.remove(&approval_id);
                let _ = crate::db::approvals::resolve_approval_strict(
                    &self.db, approval_id, "cancelled", "system",
                ).await;
                ApprovalOutcome::Cancelled
            }
            Err(_) => {
                // Timeout — attempt to mark as timed out in DB.
                // `was_pending` is true iff our UPDATE actually transitioned the
                // row pending → timeout (i.e. we won the race against any
                // concurrent webhook resolver). Both AlreadyResolved/NotFound
                // map to false; raw DB errors are logged and treated as false.
                let was_pending = match crate::db::approvals::resolve_approval_strict(
                    &self.db,
                    approval_id,
                    "timeout",
                    "system",
                )
                .await
                {
                    Ok(()) => true,
                    Err(crate::db::approvals::ApprovalError::AlreadyResolved { .. })
                    | Err(crate::db::approvals::ApprovalError::NotFound { .. }) => false,
                    Err(crate::db::approvals::ApprovalError::Db(e)) => {
                        tracing::warn!(
                            approval_id = %approval_id,
                            error = ?e,
                            "resolve_approval_strict(timeout) DB error"
                        );
                        false
                    }
                };

                // Drop the waiter. DashMap has no cross-await lock to hold, so
                // the prior "release waiters lock before acquiring sse_event_tx"
                // dance is no longer needed.
                self.waiters.remove(&approval_id);

                // If timeout raced with approval (DB already resolved), check actual DB status.
                // The webhook may have approved it just before our timeout fired.
                if !was_pending {
                    if let Ok(Some(approval)) = crate::db::approvals::get_approval(&self.db, approval_id).await
                        && approval.status == "approved"
                    {
                        tracing::info!(
                            tool = %tool_name,
                            approval_id = %approval_id,
                            "approval timeout raced with webhook — webhook won, honoring approval"
                        );
                        return ApprovalOutcome::Approved;
                    }
                    tracing::warn!(
                        tool = %tool_name,
                        approval_id = %approval_id,
                        "approval timeout raced with resolution — timeout takes precedence"
                    );
                }

                // Emit SSE event for timeout — non-text, MUST be delivered.
                // F036: target this session's sender (cloned before await).
                let timeout_sender =
                    session_id.and_then(|sid| sse_event_tx.get(&sid).map(|r| r.clone()));
                if let Some(tx) = timeout_sender
                    && let Err(e) = tx
                        .send_async(StreamEvent::ApprovalResolved {
                            approval_id,
                            action: ApprovalAction::TimeoutRejected,
                            modified_input: None,
                        })
                        .await
                {
                    tracing::warn!(approval_id = %approval_id, error = ?e, "ApprovalResolved timeout send failed");
                }

                ApprovalOutcome::TimedOut { timeout_secs }
            }
        }
    }

    /// Evict stale approval waiters (older than 10 minutes).
    ///
    /// Kept `async` for call-site stability (callers in run.rs await it).
    /// The body itself no longer requires async since DashMap's `retain` is sync.
    pub(crate) async fn prune_stale(&self) {
        let now = Instant::now();
        self.waiters.retain(|id, (_, created)| {
            let stale = now.duration_since(*created) > Duration::from_secs(600);
            if stale {
                tracing::debug!(approval_id = %id, "evicting stale approval waiter (>10min)");
            }
            !stale
        });
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn broadcast_ui(
        ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
        event: serde_json::Value,
    ) {
        if let Some(tx) = ui_event_tx {
            tx.send(event.to_string()).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    //! Regression guards for the code review fix (2026-04-17).
    //! Approval SSE events must use the async non-drop path on the bounded
    //! channel; the sync path can silently drop when the channel is Full.
    //! Patterns are constructed at runtime so the haystack (this file's own
    //! source) cannot contain them literally.
    use std::path::Path;

    fn source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/agent/approval_manager.rs");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
    }

    fn bad_pattern(variant: &str) -> String {
        format!("{}{}{}{}", ".", "send", "(StreamEvent::", variant)
    }

    fn good_pattern(variant: &str) -> String {
        format!("send_async(StreamEvent::{variant}")
    }

    #[test]
    fn approval_needed_uses_send_async() {
        let src = source();
        assert!(
            !src.contains(&bad_pattern("ApprovalNeeded")),
            "ApprovalNeeded must use send_async path; sync path silently drops on Full"
        );
        assert!(
            src.contains(&good_pattern("ApprovalNeeded")),
            "ApprovalNeeded must explicitly call send_async"
        );
    }

    #[test]
    fn approval_resolved_uses_send_async() {
        let src = source();
        assert!(
            !src.contains(&bad_pattern("ApprovalResolved")),
            "ApprovalResolved must use send_async path; sync path silently drops on Full"
        );
        assert!(
            src.contains(&good_pattern("ApprovalResolved")),
            "ApprovalResolved must explicitly call send_async"
        );
    }
}
