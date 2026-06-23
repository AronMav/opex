//! MetricsProbe — test helper that reads AtomicU64 counters by (agent, event_type).
//!
//! Plan 01 shipped the standalone probe (self-contained map for self-tests). Plan 02
//! adds `BoundMetricsProbe`: `connect(Arc<MetricsRegistry>)` binds the probe to the
//! real production registry so integration tests observe the same counters the
//! `/api/health/dashboard` handler reads from.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct MetricsProbe {
    // (agent, event_type) -> counter
    inner: Mutex<HashMap<(String, String), AtomicU64>>,
}

impl MetricsProbe {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Read counter value for (agent, event_type). Returns 0 if unknown.
    pub fn read_counter(&self, agent: &str, event_type: &str) -> u64 {
        let guard = self.inner.lock().expect("metrics probe poisoned");
        guard
            .get(&(agent.to_string(), event_type.to_string()))
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Take a snapshot of all counters.
    pub fn snapshot(&self) -> HashMap<(String, String), u64> {
        let guard = self.inner.lock().expect("metrics probe poisoned");
        guard
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect()
    }

    /// Test-only: increment a counter. Production counters are updated by
    /// src/metrics.rs (Plan 02) — this is for self-tests of the probe.
    #[allow(dead_code)]
    pub fn bump(&self, agent: &str, event_type: &str) {
        let mut guard = self.inner.lock().expect("metrics probe poisoned");
        guard
            .entry((agent.to_string(), event_type.to_string()))
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Bind this probe to a real `MetricsRegistry`. The returned `BoundMetricsProbe`
    /// reads from the registry via `snapshot_sse_drops()`. Call this from tests
    /// that need to observe counters from the production code path.
    ///
    /// Plan 02: replaces the Plan 01 no-op stub with a real registry binding.
    #[allow(dead_code)]
    pub fn connect(
        self,
        registry: std::sync::Arc<opex_core::metrics::MetricsRegistry>,
    ) -> BoundMetricsProbe {
        BoundMetricsProbe { registry }
    }
}

impl Default for MetricsProbe {
    fn default() -> Self {
        Self::new()
    }
}

/// Probe bound to a real `MetricsRegistry`. Reads from the production counter
/// surface (`MetricsRegistry::snapshot_sse_drops`), so integration tests see
/// exactly what the `/api/health/dashboard` handler sees.
pub struct BoundMetricsProbe {
    registry: std::sync::Arc<opex_core::metrics::MetricsRegistry>,
}

impl BoundMetricsProbe {
    /// Read counter value for (agent, event_type). Returns 0 if unknown.
    pub fn read_counter(&self, agent: &str, event_type: &str) -> u64 {
        self.registry
            .snapshot_sse_drops()
            .get(&(agent.to_string(), event_type.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// Snapshot of all counters in the registry.
    pub fn snapshot(&self) -> HashMap<(String, String), u64> {
        self.registry.snapshot_sse_drops()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_counter_returns_zero_before_bump() {
        let probe = MetricsProbe::new();
        assert_eq!(probe.read_counter("agent-a", "text-delta"), 0);
    }

    #[test]
    fn bump_increments_counter() {
        let probe = MetricsProbe::new();
        probe.bump("agent-a", "text-delta");
        probe.bump("agent-a", "text-delta");
        probe.bump("agent-a", "tool-call");
        assert_eq!(probe.read_counter("agent-a", "text-delta"), 2);
        assert_eq!(probe.read_counter("agent-a", "tool-call"), 1);
        assert_eq!(probe.read_counter("agent-b", "text-delta"), 0);
    }

    #[test]
    fn snapshot_returns_all_labels() {
        let probe = MetricsProbe::new();
        probe.bump("a", "x");
        probe.bump("b", "y");
        let snap = probe.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get(&("a".to_string(), "x".to_string())), Some(&1));
        assert_eq!(snap.get(&("b".to_string(), "y".to_string())), Some(&1));
    }
}
