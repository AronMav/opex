use std::sync::Arc;

use crate::gateway::state::{PollingDiagnostics, ProcessingTracker, WanIpCache};

#[derive(Clone)]
pub struct StatusMonitor {
    pub processing_tracker: ProcessingTracker,
    pub polling_diagnostics: Arc<PollingDiagnostics>,
    pub wan_ip_cache: Arc<tokio::sync::RwLock<Option<WanIpCache>>>,
    pub started_at: std::time::Instant,
}

impl StatusMonitor {
    pub fn new(
        processing_tracker: ProcessingTracker,
        polling_diagnostics: Arc<PollingDiagnostics>,
        wan_ip_cache: Arc<tokio::sync::RwLock<Option<WanIpCache>>>,
        started_at: std::time::Instant,
    ) -> Self {
        Self {
            processing_tracker,
            polling_diagnostics,
            wan_ip_cache,
            started_at,
        }
    }

    #[cfg(test)]
    pub fn test_new() -> Self {
        use std::collections::HashMap;
        Self {
            processing_tracker: Arc::new(std::sync::RwLock::new(HashMap::new())),
            polling_diagnostics: Arc::new(PollingDiagnostics::new()),
            wan_ip_cache: Arc::new(tokio::sync::RwLock::new(None)),
            started_at: std::time::Instant::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_monitor_uptime_is_non_negative() {
        let m = StatusMonitor::test_new();
        assert!(m.started_at.elapsed().as_secs() < 5);
    }

    #[test]
    fn status_monitor_clone_shares_diagnostics() {
        let m = StatusMonitor::test_new();
        m.polling_diagnostics.record_inbound();
        let m2 = m.clone();
        assert_eq!(
            m2.polling_diagnostics
                .messages_in
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
}
