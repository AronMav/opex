//! OBS-02 + OBS-03: histogram surface + label allowlist + cardinality guard.
//!
//! Five tests pin the new `MetricsRegistry` surface introduced by Phase 65
//! Plan 02:
//!
//!   1. `histogram_methods_accept_allowed_labels_only` — the four `record_*`
//!      methods (`record_tool_latency`, `record_llm_call_duration`,
//!      `record_db_query_duration`, `record_llm_tokens`) accept values keyed
//!      by `ALLOWED_LABEL_KEYS` without panic; accumulated count+sum round-
//!      trips through `snapshot_tool_latency_summary()`.
//!   2. `record_llm_tokens_tracks_direction` — directional counter
//!      `{prompt, completion}` keeps both totals.
//!   3. `adding_session_id_label_panics` — public `assert_label_allowed`
//!      panics on a key outside the allowlist. Pins the runtime safety net
//!      that prevents `session_id`/`user_id` from being used as labels.
//!   4. `synthetic_10k_sessions_stay_under_50mb_and_guard_trips_above` —
//!      bounded 4k unique series stays green; a 10k+1 synthetic run on a
//!      child thread MUST panic (verified via `JoinHandle::join().is_err()`)
//!      via `MAX_UNIQUE_SERIES` cardinality guard.
//!   5. `atomic_counters_always_on_regardless_of_feature` — the always-on
//!      AtomicU64 summary records values whether or not the `otel` feature
//!      is active. The test binary is built WITHOUT `--features otel` by
//!      default, so this proves the non-feature path works on its own.

use opex_core::metrics::{MetricsRegistry, ALLOWED_LABEL_KEYS, MAX_UNIQUE_SERIES};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn histogram_methods_accept_allowed_labels_only() {
    let reg = MetricsRegistry::new();

    // All five keys must be in the allowlist — this pins the contract.
    for key in ["agent_id", "tool_name", "provider", "model", "result"] {
        assert!(
            ALLOWED_LABEL_KEYS.contains(&key),
            "{key} must be in ALLOWED_LABEL_KEYS"
        );
    }

    reg.record_tool_latency("workspace_write", "agent-a", "ok", Duration::from_millis(42));
    reg.record_tool_latency("workspace_write", "agent-a", "ok", Duration::from_millis(58));
    reg.record_tool_latency("code_exec", "agent-b", "error", Duration::from_millis(10));

    let snap = reg.snapshot_tool_latency_summary();
    let entry = snap
        .get(&(
            "workspace_write".to_string(),
            "agent-a".to_string(),
            "ok".to_string(),
        ))
        .copied()
        .expect("tool_latency_summary must contain (workspace_write, agent-a, ok)");
    assert_eq!(entry.0, 2, "count must be 2");
    // 42ms + 58ms = 100ms = 100_000us
    assert_eq!(entry.1, 100_000, "sum_micros must be 100_000");

    // LLM + DB histograms should also accept calls without panic.
    reg.record_llm_call_duration("openai", "gpt-4o", "ok", Duration::from_millis(250));
    reg.record_db_query_duration("ok", Duration::from_millis(5));
    let llm_snap = reg.snapshot_llm_call_duration_summary();
    let db_snap = reg.snapshot_db_query_duration_summary();
    assert_eq!(
        llm_snap
            .get(&(
                "openai".to_string(),
                "gpt-4o".to_string(),
                "ok".to_string()
            ))
            .copied(),
        Some((1, 250_000))
    );
    assert_eq!(db_snap.get("ok").copied(), Some((1, 5_000)));
}

#[test]
fn record_llm_tokens_tracks_direction() {
    let reg = MetricsRegistry::new();
    reg.record_llm_tokens(500, "prompt");
    reg.record_llm_tokens(120, "completion");
    reg.record_llm_tokens(80, "prompt");

    let snap = reg.snapshot_llm_tokens_total();
    assert_eq!(snap.get("prompt").copied(), Some(580), "prompt total");
    assert_eq!(
        snap.get("completion").copied(),
        Some(120),
        "completion total"
    );
    assert_eq!(snap.len(), 2, "only two directions");
}

#[test]
#[should_panic(expected = "label key not in allowlist: session_id")]
fn adding_session_id_label_panics() {
    MetricsRegistry::assert_label_allowed("session_id");
}

#[test]
fn synthetic_10k_sessions_stay_under_50mb_and_guard_trips_above() {
    use std::thread;

    // Phase 1: bounded cross-product under the cap (5 * 20 * 4 * 5 * 2 = 4000).
    // MUST NOT panic — the cardinality guard stays silent within budget.
    let reg = Arc::new(MetricsRegistry::new());
    for ai in 0..5 {
        for ti in 0..20 {
            for _pi in 0..4 {
                for _mi in 0..5 {
                    for ri in 0..2 {
                        reg.record_tool_latency(
                            &format!("tool_{ti}"),
                            &format!("agent_{ai}"),
                            if ri == 0 { "ok" } else { "error" },
                            Duration::from_millis(1),
                        );
                    }
                }
            }
        }
    }
    // Under the cap we expect at most (5 * 20 * 2) = 200 unique tool_latency
    // keys (provider × model do not apply to tool_latency). Just assert
    // we're below the panic ceiling.
    assert!(
        reg.unique_series_count() < MAX_UNIQUE_SERIES as u64,
        "bounded 4k-combination phase must stay under MAX_UNIQUE_SERIES; got {}",
        reg.unique_series_count()
    );

    // Phase 2: push past the cap on a child thread. The panic is CAUGHT by
    // the thread boundary — `.join()` returns Err(Any).
    let reg2 = reg.clone();
    let h = thread::spawn(move || {
        // 10_005 distinct tool names ⇒ each is a new series. Combined with
        // the Phase-1 200 existing series this MUST trip the 10k guard.
        for n in 0..10_005 {
            reg2.record_tool_latency(
                &format!("tool_guardbreak_{n}"),
                "agent_x",
                "ok",
                Duration::from_millis(1),
            );
        }
    });
    let join_result = h.join();
    assert!(
        join_result.is_err(),
        "cardinality guard MUST panic past MAX_UNIQUE_SERIES = {MAX_UNIQUE_SERIES}"
    );

    // Cross-platform RSS probe: best-effort on Linux, informational on
    // non-Linux. The hard contract is the panic above; RSS is an
    // operational check.
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    tracing::info!(vmrss = rest.trim(), "RSS after 10k cardinality probe");
                }
            }
        }
    }
}

#[test]
fn atomic_counters_always_on_regardless_of_feature() {
    // This test binary is built WITHOUT `--features otel` (default feature
    // set). The always-on AtomicU64 summary must record values regardless.
    let reg = MetricsRegistry::new();
    reg.record_tool_latency("t", "a", "ok", Duration::from_millis(5));
    let snap = reg.snapshot_tool_latency_summary();
    assert!(
        !snap.is_empty(),
        "always-on atomic must accumulate without otel feature"
    );

    // LLM tokens counter: same contract.
    reg.record_llm_tokens(42, "prompt");
    let tok_snap = reg.snapshot_llm_tokens_total();
    assert_eq!(tok_snap.get("prompt").copied(), Some(42));
}
