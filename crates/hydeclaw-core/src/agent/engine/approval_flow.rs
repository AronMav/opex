//! REF-01 Task 3: `ApprovalResult` enum + `AgentEngine::needs_approval` +
//! `AgentEngine::resolve_approval` — the engine-side approval glue consumed
//! by `src/agent/approval_manager.rs` via `pub use` re-exports in
//! `engine/mod.rs`.
//!
//! Extracted from `engine/mod.rs` as part of plan 66-02. Public API is
//! preserved byte-identically via `pub use self::approval_flow::ApprovalResult`
//! in `engine/mod.rs`.

use hydeclaw_types::ids::ApprovalId;

use super::AgentEngine;

/// Result of a tool-call approval request.
#[derive(Debug)]
pub enum ApprovalResult {
    Approved,
    ApprovedWithModifiedArgs(serde_json::Value),
    Rejected(String),
}

impl AgentEngine {
    /// Check if a tool requires approval before execution.
    pub(super) fn needs_approval(&self, tool_name: &str) -> bool {
        crate::agent::pipeline::dispatch::needs_approval(self.cfg().agent.approval.as_ref(), tool_name)
    }

    /// Resolve a pending approval (called from API/callback handler).
    pub async fn resolve_approval(
        &self,
        approval_id: ApprovalId,
        approved: bool,
        resolved_by: &str,
        modified_input: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: self.cfg(),
            state: self.state(),
            tex: self.tex(),
            subagent_depth: 0,
        };
        crate::agent::pipeline::approval::resolve_approval(&ctx, approval_id, approved, resolved_by, modified_input).await
    }
}
