use crate::config::CompactionConfig;
use serde::{Deserialize, Serialize};

/// Hard safety-valve fraction of the model's context window. Once the prompt is
/// within `1 - HARD_COMPACT_CEILING` of the window, overflow (provider 413/5xx
/// or SILENT truncation) is imminent, so compaction fires even if the anti-thrash
/// gate would otherwise skip it. Set well above the usual `threshold` (0.75) so
/// it only ever engages in the danger zone.
const HARD_COMPACT_CEILING: f64 = 0.92;

// ── Persisted state ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompressorState {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub compression_count: u32,
}

// ── Runtime struct ─────────────────────────────────────────────────────────

pub struct Compressor {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub last_prompt_tokens: u32,
    pub compression_count: u32,
    pub context_limit: u32,
}

impl Compressor {
    pub fn new(context_limit: u32) -> Self {
        Self {
            previous_summary: None,
            ineffective_count: 0,
            last_prompt_tokens: 0,
            compression_count: 0,
            context_limit,
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
        // Hard safety-valve: within ~8% of the window, a marginally-ineffective
        // compaction is strictly better than a dead turn. Bypass the anti-thrash
        // gate so a long tool-heavy conversation whose recent compactions each
        // saved <min_savings cannot trip `ineffective_count >= max_skips` and then
        // grow UNBOUNDED until the provider rejects it (413/5xx) or silently
        // truncates. Below this ceiling the anti-thrash gate still applies.
        let hard_ceiling = (self.context_limit as f64 * HARD_COMPACT_CEILING) as u32;
        if self.last_prompt_tokens >= hard_ceiling {
            tracing::warn!(
                prompt_tokens = self.last_prompt_tokens,
                context_limit = self.context_limit,
                "context near window limit — forcing compaction despite anti-thrash gate"
            );
            return true;
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
    fn hard_ceiling_overrides_anti_thrash_near_window() {
        let mut c = Compressor::new(128_000);
        let cfg = cfg(0.75);
        // Trip the anti-thrash gate with two ineffective compressions.
        c.last_prompt_tokens = 100_000;
        c.record_compression_result(100_000, 98_000, &cfg); // saved 2% < 10%
        c.record_compression_result(98_000, 96_500, &cfg); // saved 1.5% < 10%
        assert_eq!(c.ineffective_count, 2);
        // Below the hard ceiling (128_000 * 0.92 = 117_760) the gate still skips.
        assert!(!c.should_compress(&cfg), "anti-thrash gate applies below ceiling");
        // Context grows into the danger zone (> 92% of the window): overflow is
        // imminent, so compaction must fire despite the anti-thrash gate.
        c.last_prompt_tokens = 120_000;
        assert!(
            c.should_compress(&cfg),
            "hard ceiling must override anti-thrash to avoid unbounded growth → provider 5xx"
        );
    }

    #[test]
    fn hard_ceiling_still_respects_disabled_flag() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 127_000; // well past the ceiling
        let mut disabled = cfg(0.75);
        disabled.enabled = false;
        assert!(
            !c.should_compress(&disabled),
            "disabled compaction must never fire, even near the window limit"
        );
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
    fn compressor_state_serializes_without_pending_split() {
        let s = CompressorState {
            previous_summary: None,
            ineffective_count: 0,
            compression_count: 0,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("pending_split"), "field must be gone: {json}");
    }

    // Old JSON with pending_split field deserializes without error (forward compat)
    #[test]
    fn load_tolerates_old_json_with_pending_split() {
        let old_json = serde_json::json!({
            "previous_summary": null,
            "ineffective_count": 0,
            "compression_count": 2,
            "pending_split": true
        });
        let c = Compressor::load(Some(old_json), 128_000);
        assert_eq!(c.compression_count, 2);
    }

    #[test]
    fn compressor_state_default_has_zero_counters() {
        let state = CompressorState::default();
        assert_eq!(state.ineffective_count, 0);
        assert_eq!(state.compression_count, 0);
        assert!(state.previous_summary.is_none());
    }

    #[test]
    fn child_state_resets_counters_preserves_summary() {
        let parent_state = CompressorState {
            previous_summary: Some("summary".to_string()),
            ineffective_count: 3,
            compression_count: 7,
        };
        let child_state = CompressorState {
            previous_summary: parent_state.previous_summary.clone(),
            ..Default::default()
        };
        assert_eq!(child_state.ineffective_count, 0);
        assert_eq!(child_state.compression_count, 0);
        assert_eq!(child_state.previous_summary, Some("summary".to_string()));
    }

    #[test]
    fn child_at_max_ineffective_count_parent_gets_reset() {
        let default_cfg = cfg(0.75);
        let parent_state = CompressorState {
            previous_summary: Some("summary text".to_string()),
            ineffective_count: default_cfg.anti_thrash_max_skips,
            compression_count: 5,
        };
        let child_state = CompressorState {
            previous_summary: parent_state.previous_summary.clone(),
            ..Default::default()
        };
        assert_eq!(child_state.ineffective_count, 0, "child must not inherit parent's exhausted ineffective_count");
        assert_eq!(child_state.compression_count, 0);
        assert!(child_state.ineffective_count < default_cfg.anti_thrash_max_skips);
    }

    #[test]
    fn child_state_no_summary_all_defaults() {
        let child_state = CompressorState {
            previous_summary: None,
            ..Default::default()
        };
        assert!(child_state.previous_summary.is_none());
        assert_eq!(child_state.ineffective_count, 0);
        assert_eq!(child_state.compression_count, 0);
    }
}
