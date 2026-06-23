//! Bounded audit event queue — replaces fire-and-forget `tokio::spawn` for
//! tool execution and quality recording.

use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

// ── Event types ──────────────────────────────────────────────────────

/// Audit events dispatched to the background worker.
#[derive(Debug)]
pub enum AuditEvent {
    ToolExecution {
        agent_name: String,
        session_id: Option<Uuid>,
        tool_name: String,
        parameters: Option<serde_json::Value>,
        status: String,
        duration_ms: Option<i32>,
        error: Option<String>,
    },
    ToolQuality {
        tool_name: String,
        success: bool,
        duration_ms: i32,
        error: Option<String>,
    },
}

// ── Queue ────────────────────────────────────────────────────────────

/// Bounded channel + single background worker for persisting audit events.
#[derive(Debug)]
pub struct AuditQueue {
    tx: mpsc::Sender<AuditEvent>,
}

impl AuditQueue {
    /// Create the queue and spawn the drain worker.
    ///
    /// Capacity is 1024 events; callers that exceed backpressure see a
    /// warning log and the event is dropped (non-blocking).
    pub fn new(db: PgPool) -> Self {
        let (tx, mut rx) = mpsc::channel::<AuditEvent>(1024);

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    AuditEvent::ToolExecution {
                        agent_name,
                        session_id,
                        tool_name,
                        parameters,
                        status,
                        duration_ms,
                        error,
                    } => {
                        if let Err(e) = crate::db::tool_audit::record_tool_execution(
                            &db,
                            &agent_name,
                            session_id,
                            &tool_name,
                            parameters.as_ref(),
                            &status,
                            duration_ms,
                            error.as_deref(),
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "audit queue: failed to record tool execution");
                        }
                    }
                    AuditEvent::ToolQuality {
                        tool_name,
                        success,
                        duration_ms,
                        error,
                    } => {
                        if let Err(e) = crate::db::tool_quality::record_tool_result(
                            &db,
                            &tool_name,
                            success,
                            duration_ms,
                            error.as_deref(),
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "audit queue: failed to record tool quality");
                        }
                    }
                }
            }
        });

        Self { tx }
    }

    /// Enqueue an event without blocking. Drops the event on backpressure.
    pub fn send(&self, event: AuditEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("audit queue full — dropping event");
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!("audit worker dead — audit events permanently lost");
            }
        }
    }
}
