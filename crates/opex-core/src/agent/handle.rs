use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use super::channel_actions::ChannelActionRouter;
use super::engine::AgentEngine;
use crate::scheduler::Scheduler;
use crate::shutdown::DrainableAgent;

/// Runtime handle for a running agent — holds everything needed to stop it gracefully.
pub struct AgentHandle {
    pub engine: Arc<AgentEngine>,
    /// Scheduler job UUIDs registered for this agent (heartbeat).
    pub scheduler_job_ids: Vec<Uuid>,
    /// Multi-channel router — WS handlers subscribe via `router.subscribe()`.
    pub channel_router: Option<ChannelActionRouter>,
}

impl AgentHandle {
    /// Gracefully stop all agent tasks: cancel subagents, remove scheduler jobs.
    pub async fn shutdown(mut self, scheduler: &Scheduler) {
        let agent_name = &self.engine.cfg().agent.name;

        // Cancel all running subagents (REL-05)
        let all = self.engine.subagent_registry().list_summary().await;
        let mut cancelled_count = 0u32;
        for sa in &all {
            if sa.status == crate::agent::subagent_state::SubagentStatus::Running
                && let Some(handle) = self.engine.subagent_registry().get(&sa.id).await
            {
                let h = handle.read().await;
                h.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                cancelled_count += 1;
                tracing::info!(agent = %agent_name, subagent = %sa.id, "cancelled subagent on shutdown");
            }
        }
        if cancelled_count > 0 {
            tracing::info!(agent = %agent_name, count = cancelled_count, "cancelled running subagents");
        }

        // Remove scheduler jobs (heartbeat)
        for uuid in self.scheduler_job_ids.drain(..) {
            if let Err(e) = scheduler.remove_job(uuid).await {
                tracing::warn!(agent = %agent_name, job = %uuid, error = %e, "failed to remove scheduler job");
            }
        }

        tracing::info!(agent = %agent_name, "agent stopped");
    }
}

// ── DrainableAgent impl for Phase 62 RES-05 ──────────────────────────────
//
// Wires the real `AgentHandle` into `shutdown::drain_agents_with_scheduler`.
// Keeps the concrete `AgentEngine` off the `crate::shutdown` module so the
// lib facade can re-export `shutdown` without cascading the agent subtree.

impl DrainableAgent for AgentHandle {
    type Scheduler = Scheduler;
    type EngineRef = Arc<AgentEngine>;

    fn engine_ref(&self) -> Self::EngineRef {
        self.engine.clone()
    }

    fn cancel_all_requests(engine: &Self::EngineRef) {
        engine.state.cancel_all_requests();
    }

    async fn wait_drain_for(engine: &Self::EngineRef, timeout: Duration) {
        engine.state.wait_drain(timeout).await;
    }

    async fn shutdown(self, scheduler: &Self::Scheduler) {
        AgentHandle::shutdown(self, scheduler).await;
    }
}
