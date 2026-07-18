use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

/// Configuration for the tool execution loop.
#[derive(Debug, Clone)]
pub struct ToolLoopConfig {
    pub max_iterations: usize,
    pub compact_on_overflow: bool,
    pub detect_loops: bool,
    pub break_threshold: usize,
    pub max_consecutive_failures: usize,
    pub max_auto_continues: u8,
    pub max_loop_nudges: usize,
    pub error_break_threshold: usize,
    /// Sliding window (most-recent N tool calls) scanned for a repeated
    /// identical call that is NOT consecutive — catches "alternating" loops the
    /// `break_threshold` (consecutive) counter misses, e.g. A,B,A,B,A,B where the
    /// model ping-pongs between two tools (or a tool + a "thinking" call) with
    /// the same args each time. 0 disables the window scan.
    pub loop_window_size: usize,
    /// How many times the SAME (tool,args) hash may appear within
    /// `loop_window_size` before it is treated as a loop. Break fires on the
    /// occurrence that reaches this count. 0 disables the window scan.
    pub loop_window_repeat_threshold: usize,
}

/// Default sliding-window span for the non-consecutive repeat scan.
pub const DEFAULT_LOOP_WINDOW_SIZE: usize = 24;
/// Default repeat count within the window that trips a loop break.
pub const DEFAULT_LOOP_WINDOW_REPEAT_THRESHOLD: usize = 6;

impl ToolLoopConfig {
    pub fn effective_max_iterations(&self) -> usize {
        if self.max_iterations == 0 { usize::MAX } else { self.max_iterations }
    }
}

impl Default for ToolLoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            compact_on_overflow: true,
            detect_loops: true,
            break_threshold: 10,
            max_consecutive_failures: 3,
            max_auto_continues: 5,
            max_loop_nudges: 3,
            error_break_threshold: 3,
            loop_window_size: DEFAULT_LOOP_WINDOW_SIZE,
            loop_window_repeat_threshold: DEFAULT_LOOP_WINDOW_REPEAT_THRESHOLD,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum LoopStatus {
    Ok,
    Break(String),
}

/// Detects repetitive tool call patterns with two-phase checking.
pub struct LoopDetector {
    recent: VecDeque<u64>,
    recent_names: VecDeque<String>,
    consecutive: usize,
    last_hash: Option<u64>,
    break_threshold: usize,
    tool_counts: HashMap<String, usize>,
    consecutive_errors: usize,
    last_error_tool: Option<String>,
    error_break_threshold: usize,
    loop_window_size: usize,
    loop_window_repeat_threshold: usize,
}

impl LoopDetector {
    pub fn new(config: &ToolLoopConfig) -> Self {
        Self {
            recent: VecDeque::with_capacity(64),
            recent_names: VecDeque::with_capacity(64),
            consecutive: 0,
            last_hash: None,
            break_threshold: config.break_threshold,
            tool_counts: HashMap::new(),
            consecutive_errors: 0,
            last_error_tool: None,
            error_break_threshold: config.error_break_threshold,
            loop_window_size: config.loop_window_size,
            loop_window_repeat_threshold: config.loop_window_repeat_threshold,
        }
    }

    /// Count how many of the most-recent `loop_window_size` recorded calls share
    /// `hash`. Used by [`check_limits`] to catch non-consecutive (alternating)
    /// repeats that the consecutive counter resets away.
    fn window_repeat_count(&self, hash: u64) -> usize {
        if self.loop_window_size == 0 {
            return 0;
        }
        self.recent
            .iter()
            .rev()
            .take(self.loop_window_size)
            .filter(|&&h| h == hash)
            .count()
    }

    /// PHASE 1: Check if this call WOULD trigger a loop break. Call BEFORE execution.
    pub fn check_limits(&self, tool_name: &str, args: &serde_json::Value) -> LoopStatus {
        if !self.recent.is_empty() {
            let hash = Self::hash_call(tool_name, args);
            if self.last_hash == Some(hash) && self.consecutive + 1 >= self.break_threshold {
                return LoopStatus::Break(format!("tool '{}' called {} times consecutively", tool_name, self.consecutive + 1));
            }
            // Non-consecutive (alternating) repeat: the identical call keeps
            // recurring within the recent window even though other calls are
            // interleaved (A,B,A,B,…). The consecutive counter above resets on
            // every B, so this is the only guard that catches it.
            if self.loop_window_repeat_threshold > 0 {
                let occurrences = self.window_repeat_count(hash) + 1; // +1 = this pending call
                if occurrences >= self.loop_window_repeat_threshold {
                    return LoopStatus::Break(format!(
                        "tool '{}' called {} times with identical args within the last {} calls (alternating loop)",
                        tool_name, occurrences, self.loop_window_size
                    ));
                }
            }
        }
        LoopStatus::Ok
    }

    /// PHASE 2: Record actual execution.
    pub fn record_execution(&mut self, tool_name: &str, args: &serde_json::Value, success: bool) -> LoopStatus {
        let hash = Self::hash_call(tool_name, args);
        *self.tool_counts.entry(tool_name.to_string()).or_insert(0) += 1;

        if self.last_hash == Some(hash) {
            self.consecutive += 1;
        } else {
            self.consecutive = 1;
            self.last_hash = Some(hash);
        }

        if self.recent.len() >= 64 {
            self.recent.pop_front();
            self.recent_names.pop_front();
        }
        self.recent.push_back(hash);
        self.recent_names.push_back(tool_name.to_string());

        self.record_result(tool_name, success)
    }

    /// Replay a timeline event: restore hash-repeat state (when the row carries an
    /// `args_hash`) AND the error streak. Mirrors `record_execution`'s
    /// consecutive/`last_hash`/`recent` logic so a loop in progress before a crash
    /// keeps the SAME consecutive count after warm-up.
    fn replay_from_timeline(&mut self, tool_name: &str, args_hash: Option<u64>, success: bool) {
        if let Some(hash) = args_hash {
            if self.last_hash == Some(hash) {
                self.consecutive += 1;
            } else {
                self.consecutive = 1;
                self.last_hash = Some(hash);
            }
            if self.recent.len() >= 64 {
                self.recent.pop_front();
                self.recent_names.pop_front();
            }
            self.recent.push_back(hash);
            self.recent_names.push_back(tool_name.to_string());
        }
        let _ = self.record_result(tool_name, success);
    }

    /// Record only the result (used for timeline warm-up and after execution).
    pub fn record_result(&mut self, tool_name: &str, success: bool) -> LoopStatus {
        if success {
            self.consecutive_errors = 0;
            self.last_error_tool = None;
        } else {
            if self.last_error_tool.as_deref() == Some(tool_name) {
                self.consecutive_errors += 1;
            } else {
                self.consecutive_errors = 1;
                self.last_error_tool = Some(tool_name.to_string());
            }
            if self.consecutive_errors >= self.error_break_threshold {
                return LoopStatus::Break(format!("tool '{}' failed {} times consecutively", tool_name, self.consecutive_errors));
            }
        }
        LoopStatus::Ok
    }

    /// Reconstruct detector state from timeline tool_end events after crash/resume (BUG-026).
    ///
    /// Replays the error-streak (consecutive_errors + last_error_tool) AND — when
    /// the row carries an `args_hash` — the hash-repeat state (consecutive/last_hash/
    /// recent), so a loop in progress before the crash keeps the SAME consecutive
    /// count. The persisted hash is keyed on `loop_detector_key` (see `parallel.rs`),
    /// matching the live `check_limits`. Legacy events lacking `args_hash` fall back
    /// to error-streak only (no panic).
    pub fn warm_up_from_timeline(config: &ToolLoopConfig, events: &[opex_db::session_timeline::TimelineToolEvent]) -> Self {
        let mut detector = Self::new(config);
        for e in events {
            let hash = e
                .args_hash
                .as_deref()
                .and_then(|h| u64::from_str_radix(h, 16).ok());
            detector.replay_from_timeline(&e.tool_name, hash, e.success);
        }
        detector
    }

    pub fn hash_call_raw(name: &str, args: &serde_json::Value) -> u64 { Self::hash_call(name, args) }

    fn hash_call(name: &str, args: &serde_json::Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        let args_str = serde_json::to_string(args).unwrap_or_default();
        args_str.hash(&mut hasher);
        hasher.finish()
    }

    pub fn tool_counts(&self) -> &HashMap<String, usize> { &self.tool_counts }
    pub fn iteration_count(&self) -> usize { self.tool_counts.values().sum() }

    // NOTE: there is intentionally no `reset()` method. Loop nudges must NOT
    // clear the detector's history — see regression test
    // `loop_detector_persists_history_across_nudge` and
    // `pipeline/tool_loop_helpers.rs::apply_loop_nudge`. If you need to "reset"
    // because of a true session boundary, construct a new `LoopDetector`.
}

impl From<&crate::config::ToolLoopSettings> for ToolLoopConfig {
    fn from(s: &crate::config::ToolLoopSettings) -> Self {
        Self {
            max_iterations: s.max_iterations,
            compact_on_overflow: s.compact_on_overflow,
            detect_loops: s.detect_loops,
            break_threshold: s.break_threshold,
            max_consecutive_failures: s.max_consecutive_failures,
            max_auto_continues: s.max_auto_continues,
            max_loop_nudges: s.max_loop_nudges,
            error_break_threshold: s.error_break_threshold.unwrap_or(3),
            loop_window_size: DEFAULT_LOOP_WINDOW_SIZE,
            loop_window_repeat_threshold: DEFAULT_LOOP_WINDOW_REPEAT_THRESHOLD,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opex_db::session_timeline::TimelineToolEvent;

    fn config(threshold: usize) -> ToolLoopConfig {
        ToolLoopConfig {
            max_iterations: 100,
            compact_on_overflow: false,
            detect_loops: true,
            break_threshold: threshold,
            max_consecutive_failures: 5,
            max_auto_continues: 3,
            max_loop_nudges: 2,
            error_break_threshold: 3,
            ..ToolLoopConfig::default()
        }
    }

    /// Regression test for F4 (loop_detector.reset() removal).
    ///
    /// The loop detector must retain history across nudges. If reset() were
    /// called between nudges, the same repeating sequence would not trip on the
    /// next nudge, effectively allowing max_nudges × break_threshold iterations.
    #[test]
    fn loop_detector_persists_history_across_nudge() {
        let cfg = config(2); // trip after 2 identical consecutive calls
        let mut detector = LoopDetector::new(&cfg);
        let args = serde_json::json!({});

        // First call: no trip
        assert!(matches!(detector.check_limits("tool", &args), LoopStatus::Ok));
        detector.record_execution("tool", &args, true);

        // Second call: trips (consecutive == 2 >= break_threshold)
        let status = detector.check_limits("tool", &args);
        assert!(matches!(status, LoopStatus::Break(_)), "should break after {threshold} consecutive identical calls", threshold = cfg.break_threshold);

        // After a nudge (WITHOUT reset), the detector still has history.
        // A third identical call must still trip immediately.
        let status2 = detector.check_limits("tool", &args);
        assert!(matches!(status2, LoopStatus::Break(_)), "detector must retain history after nudge — no reset() allowed");
    }

    /// The Arty failure mode: the model ping-pongs A,B,A,B,… with identical args,
    /// so the CONSECUTIVE counter resets on every B and never trips. The sliding
    /// window scan must still catch it once A recurs `loop_window_repeat_threshold`
    /// times within `loop_window_size`.
    #[test]
    fn alternating_loop_is_detected_by_window_scan() {
        // High consecutive threshold so ONLY the window scan can fire.
        let cfg = ToolLoopConfig { break_threshold: 100, ..ToolLoopConfig::default() };
        let mut d = LoopDetector::new(&cfg);
        let a = serde_json::json!({"thought": "same"});
        let b = serde_json::json!({"skill": "web-search"});
        // Record 5 interleaved A/B pairs — A appears 5× (default threshold is 6).
        for _ in 0..5 {
            assert!(matches!(d.check_limits("A", &a), LoopStatus::Ok));
            d.record_execution("A", &a, true);
            d.record_execution("B", &b, true); // resets the consecutive streak
        }
        // The 6th identical A within the window trips the alternating-loop guard.
        assert!(
            matches!(d.check_limits("A", &a), LoopStatus::Break(_)),
            "A,B,A,B… with identical args must trip the window scan"
        );
    }

    #[test]
    fn alternating_below_threshold_is_ok() {
        let cfg = ToolLoopConfig { break_threshold: 100, ..ToolLoopConfig::default() };
        let mut d = LoopDetector::new(&cfg);
        let a = serde_json::json!({"x": 1});
        let b = serde_json::json!({"y": 2});
        for _ in 0..3 {
            d.record_execution("A", &a, true);
            d.record_execution("B", &b, true);
        }
        assert!(
            matches!(d.check_limits("A", &a), LoopStatus::Ok),
            "3 repeats is below the window threshold (6) — must not trip"
        );
    }

    /// Same tool with DIFFERENT args each time (distinct hashes) is legitimate
    /// progress (e.g. sequentialthinking advancing thoughtNumber, or reading
    /// different files) and must never trip the window scan.
    #[test]
    fn distinct_args_do_not_false_positive() {
        let cfg = ToolLoopConfig { break_threshold: 100, ..ToolLoopConfig::default() };
        let mut d = LoopDetector::new(&cfg);
        for i in 0..20 {
            let args = serde_json::json!({ "n": i });
            assert!(
                matches!(d.check_limits("tool", &args), LoopStatus::Ok),
                "distinct-arg calls must never trip the window scan"
            );
            d.record_execution("tool", &args, true);
        }
    }

    #[test]
    fn warm_up_from_timeline_restores_error_streak() {
        let cfg = config(3); // error_break_threshold = 3
        let events = vec![
            TimelineToolEvent { tool_name: "fs".to_string(), success: false, args_hash: None },
            TimelineToolEvent { tool_name: "fs".to_string(), success: false, args_hash: None },
        ];
        let mut detector = LoopDetector::warm_up_from_timeline(&cfg, &events);
        let status = detector.record_result("fs", false);
        assert!(
            matches!(status, LoopStatus::Break(_)),
            "error streak should be restored from timeline — 2 prior failures + 1 new = trip at threshold 3"
        );
    }

    #[test]
    fn warm_up_from_timeline_restores_hash_repeat_detection() {
        let cfg = config(3); // break_threshold = 3
        let args = serde_json::json!({"q": "x"});
        // Live path keys on loop_detector_key; here the direct tool name == its key.
        let h = format!("{:x}", LoopDetector::hash_call_raw("web_search", &args));
        // Three identical successful calls already happened before the crash.
        let events = vec![
            TimelineToolEvent { tool_name: "web_search".into(), success: true, args_hash: Some(h.clone()) },
            TimelineToolEvent { tool_name: "web_search".into(), success: true, args_hash: Some(h.clone()) },
            TimelineToolEvent { tool_name: "web_search".into(), success: true, args_hash: Some(h.clone()) },
        ];
        let detector = LoopDetector::warm_up_from_timeline(&cfg, &events);
        // The next identical call must break NOW (consecutive already 3 >= threshold).
        assert!(
            matches!(detector.check_limits("web_search", &args), LoopStatus::Break(_)),
            "hash-repeat detection must survive warm-up (today it does NOT)"
        );
    }

    #[test]
    fn warm_up_tolerates_legacy_events_without_args_hash() {
        let cfg = config(3);
        let events = vec![
            TimelineToolEvent { tool_name: "fs".into(), success: false, args_hash: None },
            TimelineToolEvent { tool_name: "fs".into(), success: false, args_hash: None },
        ];
        let mut detector = LoopDetector::warm_up_from_timeline(&cfg, &events);
        // Error streak still restored (2 + 1 = trip at 3); no panic on missing hash.
        assert!(matches!(detector.record_result("fs", false), LoopStatus::Break(_)));
    }

    #[test]
    fn warm_up_from_timeline_empty_events_gives_fresh_detector() {
        let cfg = config(3);
        let events: Vec<TimelineToolEvent> = vec![];
        let mut detector = LoopDetector::warm_up_from_timeline(&cfg, &events);
        // Two failures — should NOT trip yet (only 2 of 3 threshold)
        detector.record_result("tool", false);
        let status = detector.record_result("tool", false);
        assert!(matches!(status, LoopStatus::Ok), "empty timeline should produce fresh detector");
    }

    #[test]
    fn warm_up_from_timeline_success_resets_streak() {
        let cfg = config(3);
        let events = vec![
            TimelineToolEvent { tool_name: "tool".to_string(), success: false, args_hash: None },
            TimelineToolEvent { tool_name: "tool".to_string(), success: false, args_hash: None },
            TimelineToolEvent { tool_name: "tool".to_string(), success: true, args_hash: None }, // success resets
        ];
        let mut detector = LoopDetector::warm_up_from_timeline(&cfg, &events);
        // After a success reset, two more failures should not trip
        detector.record_result("tool", false);
        let status = detector.record_result("tool", false);
        assert!(matches!(status, LoopStatus::Ok), "success in timeline should reset error streak");
    }
}
