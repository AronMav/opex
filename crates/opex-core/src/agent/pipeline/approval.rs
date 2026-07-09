//! Pipeline step: approval resolution.
//! Extracted from engine.rs as a free function taking &CommandContext.

use super::CommandContext;
use opex_types::approvals::ApprovalAction;
use opex_types::ids::ApprovalId;
use crate::agent::engine::{ApprovalResult, StreamEvent};

/// Resolve a pending approval (called from API/callback handler).
pub async fn resolve_approval(
    ctx: &CommandContext<'_>,
    approval_id: ApprovalId,
    approved: bool,
    resolved_by: &str,
    modified_input: Option<serde_json::Value>,
) -> anyhow::Result<()> {
    let status = if approved { "approved" } else { "rejected" };
    // Phase 63 DATA-04: switch to the transactional strict variant so we can
    // surface typed outcomes. Distinct bail! messages let `api_resolve_approval`
    // pattern-match on the anyhow root cause when deciding HTTP status.
    match crate::db::approvals::resolve_approval_strict(
        &ctx.cfg.db,
        approval_id,
        status,
        resolved_by,
    )
    .await
    {
        Ok(()) => { /* fall through to downstream audit/SSE/waiter logic */ }
        Err(crate::db::approvals::ApprovalError::NotFound { id }) => {
            anyhow::bail!("approval {id} not found");
        }
        Err(crate::db::approvals::ApprovalError::AlreadyResolved { id, status: current }) => {
            anyhow::bail!("approval {id} already resolved (status={current})");
        }
        Err(crate::db::approvals::ApprovalError::Db(e)) => {
            return Err(anyhow::Error::from(e).context("resolve_approval_strict DB error"));
        }
    }

    crate::agent::pipeline::llm_call::audit(
        ctx.cfg.db.clone(),
        ctx.cfg.agent.name.clone(),
        crate::db::audit::event_types::APPROVAL_RESOLVED,
        Some(resolved_by),
        serde_json::json!({
            "approval_id": approval_id.to_string(), "status": status
        }),
    );

    if let Some(ref tx) = ctx.state.ui_event_tx {
        tx.send(serde_json::json!({
            "type": "approval_resolved",
            "approval_id": approval_id.to_string(),
            "agent": ctx.cfg.agent.name,
            "status": status,
        }).to_string()).ok();
    }

    // Emit SSE event for inline approval resolution in chat UI.
    // ApprovalResolved is non-text and MUST be delivered (the client is
    // actively waiting on this event); use send_async to honor the
    // EngineEventSender "non-text never dropped" contract.
    let action = if approved { ApprovalAction::Approved } else { ApprovalAction::Rejected };
    // F036: the resolve path (webhook/API callback) has no session_id in scope.
    // ApprovalResolved carries only approval_id + action (no sensitive tool
    // data) and each client acts only on the approval_id it is waiting on, so
    // deliver to every live session sender. Clone them out of the per-session
    // map first — never hold a DashMap Ref across the await.
    let senders: Vec<_> = ctx
        .tex
        .sse_event_tx
        .iter()
        .map(|r| r.value().clone())
        .collect();
    for tx in senders {
        if let Err(e) = tx
            .send_async(StreamEvent::ApprovalResolved {
                approval_id,
                action,
                modified_input: modified_input.clone(),
            })
            .await
        {
            tracing::warn!(approval_id = %approval_id, error = ?e, "ApprovalResolved send failed");
        }
    }

    // Wake up the waiting tool execution.
    // Waiters map is a DashMap — no async lock, `.remove()` returns Option<(K, V)>.
    let waiters = ctx.cfg.approval_manager.waiters();
    if let Some((_id, (tx, _created_at))) = waiters.remove(&approval_id) {
        let result = if approved {
            match modified_input {
                Some(args) => ApprovalResult::ApprovedWithModifiedArgs(args),
                None => ApprovalResult::Approved,
            }
        } else {
            ApprovalResult::Rejected(format!("rejected by {resolved_by}"))
        };
        tx.send(result).ok();
    }

    // F035: do NOT opportunistically evict other waiters by a hardcoded 300s
    // here. `approval_waiters` is per-agent and shared across all its sessions,
    // so a 300s retain force-cancelled OTHER approvals that were still
    // legitimately pending under a longer configured `timeout_seconds` (e.g.
    // 600s) — the human hadn't clicked yet, but their tool was silently
    // cancelled ~5 min early. Each waiter already enforces its OWN deadline via
    // `tokio::time::timeout(timeout_secs, result_rx)`, and `prune_stale` reaps
    // orphaned entries; this opportunistic cutoff was decoupled from both.

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Regression guard: the inline resolve_approval SSE event must be
    //! delivered via the async non-drop path. Runtime-constructed patterns
    //! prevent the test from matching its own source.
    use std::path::Path;

    #[test]
    fn approval_resolved_in_pipeline_uses_send_async() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/agent/pipeline/approval.rs");
        let src = std::fs::read_to_string(&path).expect("read pipeline/approval.rs");
        let bad = format!("{}{}{}", ".", "send", "(StreamEvent::ApprovalResolved");
        let good = "send_async(StreamEvent::ApprovalResolved";
        assert!(!src.contains(&bad), "pipeline ApprovalResolved must use async path");
        assert!(src.contains(good), "pipeline ApprovalResolved must explicitly call send_async");
    }
}
