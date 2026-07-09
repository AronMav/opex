use std::collections::HashMap;
use std::time::Instant;

pub struct RecoveryState {
    restart_times: HashMap<String, Vec<Instant>>,
    flapping: HashMap<String, bool>,
    cooldown_until: HashMap<String, Instant>,
}

impl RecoveryState {
    pub fn new() -> Self {
        Self {
            restart_times: HashMap::new(),
            flapping: HashMap::new(),
            cooldown_until: HashMap::new(),
        }
    }

    pub fn is_flapping(&self, name: &str) -> bool {
        self.flapping.get(name).copied().unwrap_or(false)
    }

    pub fn in_cooldown(&self, name: &str) -> bool {
        self.cooldown_until
            .get(name)
            .is_some_and(|t| Instant::now() < *t)
    }

    pub fn can_restart(
        &mut self,
        name: &str,
        flap_window_secs: u64,
        flap_threshold: u32,
    ) -> bool {
        // F108: flapping used to be terminal — is_flapping short-circuited before
        // in_cooldown, and flapping was only ever cleared by mark_recovered (which
        // needs the service to recover on its own). So a service that flapped and
        // stayed down could never be auto-restarted even after cooldown_secs,
        // making the configured cooldown inert. Now, once a flapping service's
        // cooldown elapses, clear the flapping state and allow a fresh attempt
        // (back off, then retry).
        if self.is_flapping(name) {
            if self.in_cooldown(name) {
                return false;
            }
            self.flapping.remove(name);
            self.cooldown_until.remove(name);
            self.restart_times.remove(name);
        } else if self.in_cooldown(name) {
            return false;
        }
        let now = Instant::now();
        let window = std::time::Duration::from_secs(flap_window_secs);
        let history = self.restart_times.entry(name.to_string()).or_default();
        history.retain(|t| now.duration_since(*t) < window);
        if history.len() >= flap_threshold as usize {
            self.flapping.insert(name.to_string(), true);
            tracing::error!(service = name, "flapping detected — stopping restarts");
            return false;
        }
        history.push(now);
        true
    }

    pub fn enter_cooldown(&mut self, name: &str, cooldown_secs: u64) {
        self.cooldown_until.insert(
            name.to_string(),
            Instant::now() + std::time::Duration::from_secs(cooldown_secs),
        );
    }

    pub fn mark_recovered(&mut self, name: &str) {
        self.restart_times.remove(name);
        self.flapping.remove(name);
        self.cooldown_until.remove(name);
    }
}

pub async fn restart_service(cmd: &str) -> bool {
    tracing::info!(cmd, "restarting service");
    let mut c = tokio::process::Command::new("bash");
    c.args(["-c", cmd]);
    match crate::proc::output_with_timeout(&mut c, crate::proc::DEFAULT_PROC_TIMEOUT).await {
        Some(o) => o.status.success(),
        None => {
            tracing::error!(cmd, "restart timed out or failed to spawn");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_allows_restart() {
        let mut state = RecoveryState::new();
        assert!(state.can_restart("svc", 600, 3));
    }

    #[test]
    fn flapping_detected_after_threshold() {
        let mut state = RecoveryState::new();
        assert!(state.can_restart("svc", 600, 3)); // 1
        assert!(state.can_restart("svc", 600, 3)); // 2
        assert!(state.can_restart("svc", 600, 3)); // 3
        assert!(!state.can_restart("svc", 600, 3)); // flapping
        assert!(state.is_flapping("svc"));
    }

    #[test]
    fn cooldown_blocks_restart() {
        let mut state = RecoveryState::new();
        state.enter_cooldown("svc", 300);
        assert!(state.in_cooldown("svc"));
        assert!(!state.can_restart("svc", 600, 3));
    }

    #[test]
    fn recovery_clears_state() {
        let mut state = RecoveryState::new();
        state.can_restart("svc", 600, 3);
        state.can_restart("svc", 600, 3);
        state.mark_recovered("svc");
        assert!(!state.is_flapping("svc"));
        // Can restart again
        assert!(state.can_restart("svc", 600, 3));
    }

    #[test]
    fn flapping_clears_after_cooldown_expires() {
        // F108: once cooldown elapses, a previously-flapping service must be
        // restartable again (was terminal before — cooldown had no effect).
        let mut state = RecoveryState::new();
        for _ in 0..3 {
            state.can_restart("svc", 600, 3);
        }
        assert!(!state.can_restart("svc", 600, 3)); // flapping detected
        assert!(state.is_flapping("svc"));
        // The main loop enters cooldown on flapping; simulate it already expired.
        state.enter_cooldown("svc", 0);
        assert!(!state.in_cooldown("svc"));
        // Cooldown elapsed → flapping cleared, restart allowed.
        assert!(state.can_restart("svc", 600, 3));
        assert!(!state.is_flapping("svc"));
    }

    #[test]
    fn flapping_still_blocks_during_cooldown() {
        // Still terminal WHILE cooling down.
        let mut state = RecoveryState::new();
        for _ in 0..3 {
            state.can_restart("svc", 600, 3);
        }
        assert!(!state.can_restart("svc", 600, 3));
        state.enter_cooldown("svc", 300);
        assert!(!state.can_restart("svc", 600, 3));
        assert!(state.is_flapping("svc"));
    }

    #[test]
    fn independent_services() {
        let mut state = RecoveryState::new();
        for _ in 0..3 { state.can_restart("a", 600, 3); }
        assert!(!state.can_restart("a", 600, 3)); // a flapping
        assert!(state.can_restart("b", 600, 3)); // b unaffected
    }
}
