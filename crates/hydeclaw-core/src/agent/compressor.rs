use crate::config::CompactionConfig;
use serde::{Deserialize, Serialize};

// ── Persisted state ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompressorState {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub compression_count: u32,
    #[serde(default)]
    pub pending_split: bool,
}

// ── Runtime struct ─────────────────────────────────────────────────────────

pub struct Compressor {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub last_prompt_tokens: u32,
    pub compression_count: u32,
    pub context_limit: u32,
    pub pending_split: bool,
}

impl Compressor {
    pub fn new(context_limit: u32) -> Self {
        Self {
            previous_summary: None,
            ineffective_count: 0,
            last_prompt_tokens: 0,
            compression_count: 0,
            context_limit,
            pending_split: false,
        }
    }

    pub fn load(state: Option<serde_json::Value>, context_limit: u32) -> Self {
        let mut c = Self::new(context_limit);
        if let Some(val) = state {
            match serde_json::from_value::<CompressorState>(val) {
                Ok(s) => {
                    c.previous_summary = s.previous_summary;
                    c.ineffective_count = s.ineffective_count;
                    c.compression_count = s.compression_count;
                    c.pending_split = s.pending_split;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to deserialize compaction_state, starting fresh");
                }
            }
        }
        c
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(CompressorState {
            previous_summary: self.previous_summary.clone(),
            ineffective_count: self.ineffective_count,
            compression_count: self.compression_count,
            pending_split: self.pending_split,
        })
        .unwrap_or(serde_json::Value::Null)
    }

    pub fn should_compress(&self, cfg: &CompactionConfig) -> bool {
        if !cfg.enabled {
            return false;
        }
        if self.last_prompt_tokens == 0 {
            return false;
        }
        let threshold = (self.context_limit as f64 * cfg.threshold) as u32;
        if self.last_prompt_tokens < threshold {
            return false;
        }
        if self.ineffective_count >= cfg.anti_thrash_max_skips {
            tracing::warn!(
                count = self.ineffective_count,
                "compression skipped — last {} compressions each saved <{:.0}% tokens; consider /new",
                self.ineffective_count,
                cfg.anti_thrash_min_savings * 100.0,
            );
            return false;
        }
        true
    }

    pub fn update_token_count(&mut self, input_tokens: u32) {
        self.last_prompt_tokens = input_tokens;
    }

    pub fn record_compression_result(
        &mut self,
        tokens_before: u32,
        tokens_after: u32,
        cfg: &CompactionConfig,
    ) {
        let savings_pct = if tokens_before > 0 {
            (tokens_before.saturating_sub(tokens_after)) as f64 / tokens_before as f64
        } else {
            0.0
        };
        if savings_pct < cfg.anti_thrash_min_savings {
            self.ineffective_count = self.ineffective_count.saturating_add(1);
        } else {
            self.ineffective_count = 0;
            self.pending_split = true;
        }
        self.compression_count = self.compression_count.saturating_add(1);
        tracing::info!(
            savings_pct = format!("{:.1}%", savings_pct * 100.0),
            compression_count = self.compression_count,
            ineffective_count = self.ineffective_count,
            "compression recorded"
        );
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(threshold: f64) -> CompactionConfig {
        CompactionConfig {
            enabled: true,
            threshold,
            anti_thrash_min_savings: 0.10,
            anti_thrash_max_skips: 2,
            ..Default::default()
        }
    }

    #[test]
    fn should_compress_false_when_no_prior_response() {
        let c = Compressor::new(128_000);
        assert!(!c.should_compress(&cfg(0.75)));
    }

    #[test]
    fn should_compress_false_below_threshold() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 50_000; // 128000 * 0.75 = 96000 → below
        assert!(!c.should_compress(&cfg(0.75)));
    }

    #[test]
    fn should_compress_true_above_threshold() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 100_000; // above 96000
        assert!(c.should_compress(&cfg(0.75)));
    }

    #[test]
    fn anti_thrash_skips_after_n_ineffective() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 100_000;
        let cfg = cfg(0.75);
        c.record_compression_result(100_000, 98_000, &cfg); // saved 2% < 10%
        c.record_compression_result(98_000, 96_500, &cfg);  // saved 1.5% < 10%
        assert_eq!(c.ineffective_count, 2);
        assert!(!c.should_compress(&cfg));
    }

    #[test]
    fn anti_thrash_resets_on_effective_compression() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 100_000;
        let cfg = cfg(0.75);
        c.record_compression_result(100_000, 98_000, &cfg); // ineffective
        c.record_compression_result(100_000, 60_000, &cfg); // saved 40% → reset
        assert_eq!(c.ineffective_count, 0);
        assert!(c.should_compress(&cfg));
    }

    #[test]
    fn load_from_none_gives_fresh_compressor() {
        let c = Compressor::load(None, 64_000);
        assert_eq!(c.context_limit, 64_000);
        assert_eq!(c.ineffective_count, 0);
        assert!(c.previous_summary.is_none());
    }

    #[test]
    fn roundtrip_state_through_json() {
        let mut c = Compressor::new(128_000);
        c.previous_summary = Some("summary text".into());
        c.ineffective_count = 1;
        c.compression_count = 3;
        let json = c.to_json();
        let c2 = Compressor::load(Some(json), 128_000);
        assert_eq!(c2.previous_summary.as_deref(), Some("summary text"));
        assert_eq!(c2.ineffective_count, 1);
        assert_eq!(c2.compression_count, 3);
    }

    #[test]
    fn pending_split_roundtrips_through_json() {
        let mut c = Compressor::new(128_000);
        c.pending_split = true;
        c.previous_summary = Some("summary".into());
        let json = c.to_json();
        let c2 = Compressor::load(Some(json), 128_000);
        assert!(c2.pending_split);
        assert_eq!(c2.previous_summary.as_deref(), Some("summary"));
    }

    #[test]
    fn pending_split_defaults_false_from_old_json_without_field() {
        let old_json = serde_json::json!({
            "previous_summary": null,
            "ineffective_count": 0,
            "compression_count": 2
        });
        let c = Compressor::load(Some(old_json), 128_000);
        assert!(!c.pending_split);
        assert_eq!(c.compression_count, 2);
    }

    #[test]
    fn record_compression_result_sets_pending_split_when_effective() {
        let mut c = Compressor::new(128_000);
        let cfg = CompactionConfig {
            enabled: true,
            threshold: 0.75,
            anti_thrash_min_savings: 0.10,
            ..Default::default()
        };
        c.record_compression_result(100_000, 60_000, &cfg);
        assert!(c.pending_split);
    }

    #[test]
    fn record_compression_result_does_not_set_pending_split_when_ineffective() {
        let mut c = Compressor::new(128_000);
        let cfg = CompactionConfig {
            enabled: true,
            threshold: 0.75,
            anti_thrash_min_savings: 0.10,
            ..Default::default()
        };
        c.record_compression_result(100_000, 98_000, &cfg);
        assert!(!c.pending_split);
    }

    // ── Unit 4: child state reset tests ────────────────────────────────────

    /// CompressorState::default() must have zero counters and no summary.
    #[test]
    fn compressor_state_default_has_zero_counters() {
        let state = CompressorState::default();
        assert_eq!(state.ineffective_count, 0);
        assert_eq!(state.compression_count, 0);
        assert!(!state.pending_split);
        assert!(state.previous_summary.is_none());
    }

    /// bootstrap.rs constructs child state with `..Default::default()` — counters reset, summary kept.
    #[test]
    fn child_state_resets_counters_preserves_summary() {
        let parent_state = CompressorState {
            previous_summary: Some("summary".to_string()),
            ineffective_count: 3,
            compression_count: 7,
            pending_split: true,
        };
        let child_state = CompressorState {
            previous_summary: parent_state.previous_summary.clone(),
            ..Default::default()
        };
        assert_eq!(child_state.ineffective_count, 0);
        assert_eq!(child_state.compression_count, 0);
        assert!(!child_state.pending_split);
        assert_eq!(child_state.previous_summary, Some("summary".to_string()));
    }

    /// Regression: parent at anti_thrash_max_skips must not infect child.
    /// Without the fix, child would inherit a maxed ineffective_count and never compress.
    #[test]
    fn child_at_max_ineffective_count_parent_gets_reset() {
        let default_cfg = cfg(0.75);
        let parent_state = CompressorState {
            previous_summary: Some("summary text".to_string()),
            ineffective_count: default_cfg.anti_thrash_max_skips,
            compression_count: 5,
            pending_split: true,
        };
        let child_state = CompressorState {
            previous_summary: parent_state.previous_summary.clone(),
            ..Default::default()
        };
        assert_eq!(child_state.ineffective_count, 0, "child must not inherit parent's exhausted ineffective_count");
        assert_eq!(child_state.compression_count, 0);
        assert!(!child_state.pending_split);
        // Child is below the threshold — can compress freely.
        assert!(child_state.ineffective_count < default_cfg.anti_thrash_max_skips);
    }

    /// Child with no previous summary: counters are clear and summary stays None.
    #[test]
    fn child_state_no_summary_all_defaults() {
        let child_state = CompressorState {
            previous_summary: None,
            ..Default::default()
        };
        assert!(child_state.previous_summary.is_none());
        assert_eq!(child_state.ineffective_count, 0);
        assert_eq!(child_state.compression_count, 0);
        assert!(!child_state.pending_split);
    }
}
