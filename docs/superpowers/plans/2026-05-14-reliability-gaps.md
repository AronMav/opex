# Reliability gaps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land two reliability features whose docs (and partial code) already exist but whose runtime behaviour does not — watchdog agent-inactivity alerts and YAML-tool response cache.

**Architecture:** Part A adds `GET /api/watchdog/agent-activity` to core (server-computed `next_expected_heartbeat_at`) and a new `inactivity.rs` module to the watchdog crate that polls the endpoint, classifies via pure functions, and dedups via an in-memory `HashMap<(agent_id, AlertType), AlertState>` (same pattern as existing `was_down` / `was_resource_warning`). Part B promotes existing `#[cfg(test)]` `ToolExecutionContext` to production, threads a process-wide `Arc<ToolExecutionContext>` through `AgentConfig` (alongside the existing `Arc<MetricsRegistry>`), wires cache lookup/write into `engine_dispatch.rs::execute_tool_call_inner` for YAML tools that have `cache:` configured (skipping `channel_action` / `pagination` / non-2xx responses).

**Tech Stack:** Rust 2024 edition, sqlx 0.8, axum 0.8, dashmap 6, cron 0.13, wiremock 0.6.5, chrono 0.4. All deps already in `Cargo.lock`; no new crates are added by this plan.

**Spec reference:** [docs/superpowers/specs/2026-05-14-reliability-gaps-design.md](../specs/2026-05-14-reliability-gaps-design.md)

**Commit policy:** Plan approval implies authorization for the `git commit` steps below — the executor SHOULD NOT prompt before each commit but MUST prompt before any `git push`, `gh pr create`, or destructive git operation (reset, force-push, branch delete). Aligns with CLAUDE.md "commit only when requested" by treating plan approval as the request.

**Pre-flight:** Confirm `DATABASE_URL` is exported (so `cargo test` runs `#[sqlx::test]` tests). Without it the endpoint integration tests in Tasks 2 and 7 are silently skipped. Run `echo $DATABASE_URL`; if empty, start the test DB via `make test-db` in a separate terminal or set `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test`.

## File map

**Created:**

- `crates/opex-core/src/gateway/handlers/monitoring/watchdog_endpoint.rs` — new file holding the `GET /api/watchdog/agent-activity` handler + supporting DB helpers.
- `crates/opex-watchdog/src/inactivity.rs` — pure-logic module: `classify`, `reconcile`, `fetch_agent_activity`, `tick`.
- `crates/opex-core/tests/integration_watchdog_agent_activity.rs` — `#[sqlx::test]` integration for the new endpoint.
- `crates/opex-watchdog/tests/integration_inactivity.rs` — wiremock-driven integration of the watchdog tick.
- `crates/opex-core/tests/integration_yaml_cache.rs` — wiremock-driven integration of cache hit/miss/bypass paths.

**Modified:**

- `crates/opex-core/src/scheduler/mod.rs` — add `compute_next_heartbeat_at(cron_expr, timezone, last_fire) -> Option<DateTime<Utc>>` helper alongside existing `compute_next_run`.
- `crates/opex-core/src/gateway/handlers/monitoring/mod.rs` — register new route.
- `crates/opex-watchdog/src/config.rs` — add `stale_activity_timeout_hours` + `missed_heartbeat_grace_minutes` fields with `serde(default)` helpers.
- `crates/opex-watchdog/src/main.rs` — allocate `inactivity_state: HashMap<EpisodeKey, AlertState>`, call `inactivity::tick(...)` per loop iteration.
- `crates/opex-watchdog/Cargo.toml` — add `wiremock` to `[dev-dependencies]` if not already there.
- `crates/opex-core/src/tools/yaml_tools.rs` — promote `ToolExecutionContext` / `CachedResponse` / `build_cache_key` from `#[cfg(test)]` to production; add `max_entries` field; swap `Mutex<HashMap>` for `DashMap`; add batch eviction.
- `crates/opex-core/src/agent/agent_config.rs` — add `pub tool_exec_ctx: Arc<crate::tools::yaml_tools::ToolExecutionContext>`.
- `crates/opex-core/src/gateway/handlers/agents/lifecycle.rs` — pass `tool_exec_ctx` into `AgentConfig` at construction.
- `crates/opex-core/src/config/mod.rs` — add `ToolCacheConfig` struct + `[tools.cache]` section.
- `crates/opex-core/src/main.rs` — construct one `Arc<ToolExecutionContext>` from config at startup, thread into `AgentDeps`-equivalent or pass through.
- `crates/opex-core/src/agent/engine_dispatch.rs` — add cache lookup + write around the YAML-tool HTTP execution.

---

## Task 1: Server-side `compute_next_heartbeat_at` helper

The endpoint in Task 2 needs to compute "next expected heartbeat after the last actual fire". The existing `compute_next_run(cron_expr, tz)` always computes "from now" and returns an RFC3339 string. Add a sibling that takes an arbitrary `after` instant and returns a typed `DateTime<Utc>`.

**Files:**

- Modify: `crates/opex-core/src/scheduler/mod.rs` (add helper near line 1555 where `compute_next_run` lives)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `scheduler/mod.rs`:

```rust
#[test]
fn compute_next_heartbeat_at_after_last_fire() {
    use chrono::TimeZone;
    // Hourly cron in Europe/Samara (UTC+4, no DST).
    let last_fire = chrono::Utc.with_ymd_and_hms(2026, 5, 14, 6, 0, 0).unwrap(); // 10:00 Samara
    let next = compute_next_heartbeat_at("0 * * * *", "Europe/Samara", last_fire);
    let expected = chrono::Utc.with_ymd_and_hms(2026, 5, 14, 7, 0, 0).unwrap(); // 11:00 Samara
    assert_eq!(next, Some(expected));
}

#[test]
fn compute_next_heartbeat_at_invalid_cron_returns_none() {
    let last_fire = chrono::Utc::now();
    let next = compute_next_heartbeat_at("not a cron expr", "Europe/Samara", last_fire);
    assert!(next.is_none());
}

#[test]
fn compute_next_heartbeat_at_handles_epoch_fallback() {
    // When the watchdog has never seen a heartbeat (last_fire = epoch start),
    // the helper must return the next-upcoming fire from epoch, not None.
    let epoch = chrono::DateTime::from_timestamp(0, 0).unwrap();
    let next = compute_next_heartbeat_at("0 * * * *", "Europe/Samara", epoch);
    assert!(next.is_some(), "must return Some(next fire) for epoch start input");
}
```

- [ ] **Step 2: Run tests, confirm they fail**

```bash
cargo test -p opex-core --bin opex-core compute_next_heartbeat_at -- --nocapture
```

Expected: FAIL with "cannot find function `compute_next_heartbeat_at`" — the helper doesn't exist yet.

- [ ] **Step 3: Implement the helper**

Add this function just below `pub fn compute_next_run(...)` (around line 1586):

```rust
/// Compute the next heartbeat fire time STRICTLY AFTER `after`, in the given
/// local timezone, returning a `DateTime<Utc>`. Used by the watchdog activity
/// endpoint to derive `next_expected_heartbeat_at` server-side so the watchdog
/// itself doesn't need the `cron` crate.
///
/// Returns `None` for an invalid cron expression.
pub fn compute_next_heartbeat_at(
    cron_expr: &str,
    timezone: &str,
    after: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    use cron::Schedule;
    use std::str::FromStr;

    let cron_6field = {
        let raw = cron_expr.trim();
        let fields: Vec<&str> = raw.split_whitespace().collect();
        if fields.len() == 5 {
            format!("0 {raw}")
        } else {
            raw.to_string()
        }
    };

    let cron_utc = convert_cron_to_utc(&cron_6field, timezone);
    let cron_7field = format!("{cron_utc} *");

    let schedule = Schedule::from_str(&cron_7field).ok()?;
    schedule.after(&after).next()
}
```

- [ ] **Step 4: Run tests, confirm they pass**

```bash
cargo test -p opex-core --bin opex-core compute_next_heartbeat_at -- --nocapture
```

Expected: PASS for all three tests.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/scheduler/mod.rs
git commit -m "$(cat <<'EOF'
feat(scheduler): compute_next_heartbeat_at helper for watchdog endpoint

Server-side variant of compute_next_run that takes an arbitrary `after`
instant and returns DateTime<Utc>. Used by the upcoming /api/watchdog/
agent-activity endpoint so the watchdog can skip cron parsing entirely.
EOF
)"
```

---

## Task 2: `GET /api/watchdog/agent-activity` endpoint

**Files:**

- Create: `crates/opex-core/src/gateway/handlers/monitoring/watchdog_endpoint.rs`
- Modify: `crates/opex-core/src/gateway/handlers/monitoring/mod.rs` (register route)
- Create: `crates/opex-core/tests/integration_watchdog_agent_activity.rs`

- [ ] **Step 1: Create the handler file with route and response shape**

Create `crates/opex-core/src/gateway/handlers/monitoring/watchdog_endpoint.rs`:

```rust
//! GET /api/watchdog/agent-activity — feeds the opex-watchdog
//! inactivity check. Returns per-agent latest activity + server-
//! computed next-expected-heartbeat so the watchdog needs no cron
//! parsing locally.

use axum::{extract::State, response::Json};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::gateway::clusters::{AgentCore, InfraServices};

#[derive(Debug, Serialize)]
pub(crate) struct AgentActivity {
    pub agent_id: String,
    pub latest_activity_at: Option<DateTime<Utc>>,
    pub next_expected_heartbeat_at: Option<DateTime<Utc>>,
}

pub(crate) async fn api_watchdog_agent_activity(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
) -> Json<Vec<AgentActivity>> {
    let map = agents.map.read().await;
    let mut out: Vec<AgentActivity> = Vec::with_capacity(map.len());

    // Every agent present in the AgentCore map is by definition "enabled":
    // it has a config file under config/agents/ that loaded successfully at
    // startup (or was added at runtime via PUT /api/agents). There is no
    // per-agent `enabled: bool` flag — removing the file is the only way
    // to disable an agent. So we iterate the whole map without filtering.
    for (name, handle) in map.iter() {
        let cfg = handle.cfg();
        // Aggregate latest activity across all sessions for this agent.
        let latest_activity_at: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT MAX(GREATEST(activity_at, last_message_at)) \
             FROM sessions WHERE agent_id = $1",
        )
        .bind(name.as_str())
        .fetch_one(&infra.db)
        .await
        .ok()
        .flatten();

        // Compute next_expected_heartbeat_at only when the agent has a
        // [agent.heartbeat] config; otherwise leave as None.
        let next_expected_heartbeat_at: Option<DateTime<Utc>> =
            if let Some(hb) = &cfg.agent.heartbeat {
                let last_heartbeat_at: Option<DateTime<Utc>> = sqlx::query_scalar(
                    "SELECT MAX(started_at) FROM sessions \
                     WHERE agent_id = $1 AND channel = 'heartbeat'",
                )
                .bind(name.as_str())
                .fetch_one(&infra.db)
                .await
                .ok()
                .flatten();
                let tz = hb.timezone.as_deref().unwrap_or("Europe/Samara");
                let after = last_heartbeat_at
                    .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
                crate::scheduler::compute_next_heartbeat_at(&hb.cron, tz, after)
            } else {
                None
            };

        out.push(AgentActivity {
            agent_id: name.clone(),
            latest_activity_at,
            next_expected_heartbeat_at,
        });
    }

    Json(out)
}
```

- [ ] **Step 2: Register the route**

Edit `crates/opex-core/src/gateway/handlers/monitoring/mod.rs`. At the top, add `mod watchdog_endpoint;` next to the existing `mod` declarations. Inside the `pub(crate) fn routes() -> Router<AppState>` definition (the function returning the `Router::new().route(...).route(...)` chain near line 43), append one route. The exact line to add:

```rust
.route("/api/watchdog/agent-activity", get(watchdog_endpoint::api_watchdog_agent_activity))
```

If you can't tell from context where to put it, place it right before the closing of the `Router::new()...` chain in the function.

- [ ] **Step 3: Write the integration test**

Create `crates/opex-core/tests/integration_watchdog_agent_activity.rs`:

```rust
//! Integration: /api/watchdog/agent-activity endpoint.

mod support;

use chrono::Utc;
use serde::Deserialize;
use sqlx::PgPool;
use support::TestHarness;

#[derive(Debug, Deserialize)]
struct AgentActivity {
    agent_id: String,
    latest_activity_at: Option<chrono::DateTime<chrono::Utc>>,
    next_expected_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[sqlx::test(migrations = "../../migrations")]
async fn endpoint_returns_per_agent_activity(pool: PgPool) {
    let harness = TestHarness::new(pool.clone()).await;
    // The harness already loads one base agent named "TestAgent" with no
    // heartbeat config. Insert two sessions: one regular, one heartbeat.
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO sessions (id, agent_id, user_id, channel, started_at, last_message_at, activity_at, run_status) \
         VALUES (gen_random_uuid(), 'TestAgent', 'u', 'web', $1, $1, $1, 'done')",
    )
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert regular session");

    let resp: Vec<AgentActivity> = harness
        .get("/api/watchdog/agent-activity")
        .await
        .expect("call endpoint");

    let agent = resp.iter().find(|a| a.agent_id == "TestAgent").expect("TestAgent present");
    assert!(agent.latest_activity_at.is_some(), "regular session bumps latest_activity_at");
    assert!(
        agent.next_expected_heartbeat_at.is_none(),
        "TestAgent has no [agent.heartbeat] config — next_expected must be None"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn endpoint_requires_auth(pool: PgPool) {
    let harness = TestHarness::new(pool.clone()).await;
    let unauth_response = harness.get_unauth("/api/watchdog/agent-activity").await;
    assert_eq!(unauth_response.status(), 401);
}
```

(If `TestHarness::get_unauth` doesn't exist, copy the request-building pattern from `tests/integration_session_timeline_cleanup.rs` and strip the `Authorization` header. Keep the assertion simple — the route is gated by the same middleware as everything else under `/api/*`.)

- [ ] **Step 4: Run the tests, confirm they pass**

```bash
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  cargo test -p opex-core --test integration_watchdog_agent_activity -- --nocapture
```

Expected: both tests PASS.

- [ ] **Step 5: Run cargo build to catch any unresolved imports**

```bash
cargo build --workspace --all-targets
```

Expected: PASS, zero errors.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/monitoring/watchdog_endpoint.rs \
        crates/opex-core/src/gateway/handlers/monitoring/mod.rs \
        crates/opex-core/tests/integration_watchdog_agent_activity.rs
git commit -m "$(cat <<'EOF'
feat(api): GET /api/watchdog/agent-activity endpoint

Returns per-agent latest_activity_at (across all session channels) and
server-computed next_expected_heartbeat_at (only for agents with
[agent.heartbeat] config). Watchdog reads this on each tick instead of
querying the DB itself — keeps the watchdog crate HTTP-only.

Auth: Bearer-token middleware (same as the rest of /api/*).
EOF
)"
```

---

## Task 3: Watchdog `inactivity.rs` pure-logic module

Pure functions only, no I/O. Unit-tested in isolation. Glue with HTTP / state happens in Task 4 and Task 5.

**Files:**

- Create: `crates/opex-watchdog/src/inactivity.rs`
- Modify: `crates/opex-watchdog/src/main.rs` (add `mod inactivity;` to the module declarations near the top)

- [ ] **Step 1: Create the module skeleton with types**

Create `crates/opex-watchdog/src/inactivity.rs`:

```rust
//! Per-agent inactivity checks (stale activity, missed heartbeat).
//! Pure logic; HTTP fetch and orchestration live in main.rs.
//!
//! Types and functions are `pub` (not `pub(crate)`) so integration
//! tests in `tests/` can drive `tick` against a wiremock server. The
//! watchdog crate is a binary — there's no public API surface to leak.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum AlertType {
    StaleActivity,
    MissedHeartbeat,
}

#[derive(Debug, Clone)]
pub struct AlertState {
    pub fired_at: DateTime<Utc>,
}

pub type EpisodeKey = (String, AlertType);

#[derive(Debug, Clone, Deserialize)]
pub struct AgentActivity {
    pub agent_id: String,
    pub latest_activity_at: Option<DateTime<Utc>>,
    pub next_expected_heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Fire {
    pub agent_id: String,
    pub alert_type: AlertType,
    pub latest_activity_at: Option<DateTime<Utc>>,
    pub next_expected_heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Recover {
    pub agent_id: String,
    pub alert_type: AlertType,
}

/// Pure classification: given one agent's activity snapshot and thresholds,
/// returns which alerts are currently firing (zero, one, or both).
///
/// There is no `enabled` check — every agent the endpoint returns is by
/// definition "loaded into AgentCore.map", which is the only meaning of
/// "enabled" in this codebase (`AgentSettings` has no `enabled` field).
pub fn classify(
    agent: &AgentActivity,
    now: DateTime<Utc>,
    stale_threshold: Duration,
    heartbeat_grace: Duration,
) -> Vec<AlertType> {
    let mut out = Vec::new();

    if let Some(latest) = agent.latest_activity_at {
        if now - latest > stale_threshold {
            out.push(AlertType::StaleActivity);
        }
    }

    if let Some(expected) = agent.next_expected_heartbeat_at {
        if now > expected + heartbeat_grace {
            out.push(AlertType::MissedHeartbeat);
        }
    }

    out
}

/// Pure dedup: walks classified results AND the set of currently-known
/// agents (so disappeared agents are silently cleaned up). Mutates state,
/// returns the events to emit.
pub fn reconcile(
    classified: HashMap<String, Vec<AlertType>>,
    activity: &HashMap<String, AgentActivity>,
    known_agents: &HashSet<String>,
    state: &mut HashMap<EpisodeKey, AlertState>,
    now: DateTime<Utc>,
) -> (Vec<Fire>, Vec<Recover>) {
    let mut fires = Vec::new();
    let mut recovers = Vec::new();

    // 1. Fires: any currently-classified alert with no open episode.
    for (agent_id, alert_types) in &classified {
        for alert_type in alert_types {
            let key = (agent_id.clone(), *alert_type);
            if state.contains_key(&key) {
                continue;
            }
            state.insert(key, AlertState { fired_at: now });
            let act = activity.get(agent_id);
            fires.push(Fire {
                agent_id: agent_id.clone(),
                alert_type: *alert_type,
                latest_activity_at: act.and_then(|a| a.latest_activity_at),
                next_expected_heartbeat_at: act.and_then(|a| a.next_expected_heartbeat_at),
            });
        }
    }

    // 2. Cleanup / recovery: walk every existing key.
    let keys_to_check: Vec<EpisodeKey> = state.keys().cloned().collect();
    for key in keys_to_check {
        let (agent_id, alert_type) = (&key.0, &key.1);
        if !known_agents.contains(agent_id) {
            // Agent renamed or deleted — silent removal, no Recover alert.
            state.remove(&key);
            continue;
        }
        let still_firing = classified
            .get(agent_id)
            .map(|v| v.contains(alert_type))
            .unwrap_or(false);
        if !still_firing {
            state.remove(&key);
            recovers.push(Recover {
                agent_id: agent_id.clone(),
                alert_type: *alert_type,
            });
        }
    }

    (fires, recovers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn agent(name: &str, latest: Option<DateTime<Utc>>, next_hb: Option<DateTime<Utc>>) -> AgentActivity {
        AgentActivity {
            agent_id: name.to_string(),
            latest_activity_at: latest,
            next_expected_heartbeat_at: next_hb,
        }
    }

    fn t(hours_ago: i64) -> DateTime<Utc> {
        Utc::now() - Duration::hours(hours_ago)
    }

    #[test]
    fn classify_stale_activity_triggers() {
        let a = agent("A", Some(t(10)), None);
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert_eq!(result, vec![AlertType::StaleActivity]);
    }

    #[test]
    fn classify_stale_activity_skips_never_active() {
        let a = agent("A", None, None);
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert!(result.is_empty());
    }

    #[test]
    fn classify_missed_heartbeat_triggers() {
        // expected 30 min ago, grace 10 min → overdue by 20 min → fire
        let a = agent("A", Some(Utc::now()), Some(Utc::now() - Duration::minutes(30)));
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert_eq!(result, vec![AlertType::MissedHeartbeat]);
    }

    #[test]
    fn classify_missed_heartbeat_respects_grace() {
        // expected 5 min ago, grace 10 min → still in grace → no fire
        let a = agent("A", Some(Utc::now()), Some(Utc::now() - Duration::minutes(5)));
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert!(result.is_empty());
    }

    #[test]
    fn classify_no_expected_heartbeat_no_alert() {
        let a = agent("A", Some(Utc::now()), None);
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert!(result.is_empty());
    }

    #[test]
    fn reconcile_fires_once() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        let mut classified: HashMap<String, Vec<AlertType>> = HashMap::new();
        classified.insert("A".to_string(), vec![AlertType::StaleActivity]);
        let activity = HashMap::from([("A".to_string(), agent("A", Some(t(10)), None))]);
        let known: HashSet<String> = ["A".to_string()].into_iter().collect();

        let (fires1, recs1) = reconcile(classified.clone(), &activity, &known, &mut state, now);
        assert_eq!(fires1.len(), 1);
        assert!(recs1.is_empty());

        let (fires2, recs2) = reconcile(classified, &activity, &known, &mut state, now);
        assert!(fires2.is_empty(), "second pass with same input must not re-fire");
        assert!(recs2.is_empty());
    }

    #[test]
    fn reconcile_recovers_on_resolution() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        state.insert(("A".to_string(), AlertType::StaleActivity), AlertState { fired_at: now });
        let activity = HashMap::from([("A".to_string(), agent("A", Some(now), None))]);
        let known: HashSet<String> = ["A".to_string()].into_iter().collect();

        let (fires, recs) = reconcile(HashMap::new(), &activity, &known, &mut state, now);
        assert!(fires.is_empty());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].alert_type, AlertType::StaleActivity);
        assert!(state.is_empty(), "state must be empty after recovery");
    }

    #[test]
    fn reconcile_independent_alert_types() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        state.insert(("A".to_string(), AlertType::StaleActivity), AlertState { fired_at: now });

        let mut classified: HashMap<String, Vec<AlertType>> = HashMap::new();
        classified.insert("A".to_string(), vec![AlertType::StaleActivity, AlertType::MissedHeartbeat]);
        let activity = HashMap::from([("A".to_string(), agent("A", Some(t(10)), Some(t(1))))]);
        let known: HashSet<String> = ["A".to_string()].into_iter().collect();

        let (fires, recs) = reconcile(classified, &activity, &known, &mut state, now);
        assert_eq!(fires.len(), 1, "stale already open, only missed_heartbeat is new");
        assert_eq!(fires[0].alert_type, AlertType::MissedHeartbeat);
        assert!(recs.is_empty());
    }

    #[test]
    fn reconcile_silent_cleanup_on_disappeared_agent() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        state.insert(("Hyde".to_string(), AlertType::StaleActivity), AlertState { fired_at: now });

        // Hyde no longer in endpoint response (renamed / deleted).
        let known: HashSet<String> = ["Alma".to_string()].into_iter().collect();
        let activity: HashMap<String, AgentActivity> = HashMap::new();

        let (fires, recs) = reconcile(HashMap::new(), &activity, &known, &mut state, now);
        assert!(fires.is_empty());
        assert!(recs.is_empty(), "silent cleanup must NOT emit Recover for vanished agent");
        assert!(state.is_empty(), "vanished agent's episode entry must be removed");
    }
}
```

- [ ] **Step 2: Register the module in `main.rs`**

Edit `crates/opex-watchdog/src/main.rs`. At the top of the file (after the existing `use` statements but before `async fn main`), add the module declaration alongside the other module declarations (you'll see `mod alerter; mod checker; mod config; mod recovery; mod resources; mod status;` or similar near the top):

```rust
mod inactivity;
```

- [ ] **Step 3: Run the unit tests, confirm they pass**

```bash
cargo test -p opex-watchdog --bin opex-watchdog inactivity -- --nocapture
```

Expected: PASS — 9 tests in `inactivity::tests` (5 classify_*, 4 reconcile_*).

- [ ] **Step 4: Run the workspace build**

```bash
cargo build --workspace --all-targets
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-watchdog/src/inactivity.rs crates/opex-watchdog/src/main.rs
git commit -m "$(cat <<'EOF'
feat(watchdog): inactivity classify + reconcile pure logic

Two pure functions: classify (one agent → alert types currently firing)
and reconcile (classification + known-agents set → Fire/Recover events,
mutates episode state). No HTTP, no DB, no I/O. Wired into the
watchdog loop in a later task.

Unit tests cover all dedup transitions including the silent-cleanup
case for renamed/deleted agents (key removed from state, no Recover
alert emitted).
EOF
)"
```

---

## Task 4: Watchdog HTTP fetch + integration test

Wraps the pure logic from Task 3 with HTTP I/O and exercises it end-to-end against a wiremock-fronted endpoint.

**Files:**

- Modify: `crates/opex-watchdog/src/inactivity.rs` (add `fetch_agent_activity` + `tick`)
- Modify: `crates/opex-watchdog/Cargo.toml` (add `wiremock` to `[dev-dependencies]` if absent)
- Create: `crates/opex-watchdog/tests/integration_inactivity.rs`

- [ ] **Step 1: Add the HTTP-fetch function and orchestration `tick`**

Append to `crates/opex-watchdog/src/inactivity.rs` (after the `tests` module — production code goes before tests; you'll need to place this code BEFORE the `#[cfg(test)] mod tests` block):

```rust
use crate::alerter::{AlertConfig, Alerter};
use crate::config::WatchdogSettings;

pub async fn fetch_agent_activity(
    http: &reqwest::Client,
    core_url: &str,
    auth_token: &str,
) -> anyhow::Result<Vec<AgentActivity>> {
    let resp = http
        .get(format!("{core_url}/api/watchdog/agent-activity"))
        .header("Authorization", format!("Bearer {auth_token}"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("agent-activity endpoint returned status {status}");
    }
    let list: Vec<AgentActivity> = resp.json().await?;
    Ok(list)
}

pub async fn tick(
    http: &reqwest::Client,
    core_url: &str,
    auth_token: &str,
    cfg: &WatchdogSettings,
    state: &mut HashMap<EpisodeKey, AlertState>,
    alerter: &Alerter,
    alert_config: &AlertConfig,
) -> anyhow::Result<()> {
    let activity = fetch_agent_activity(http, core_url, auth_token).await?;

    let now = Utc::now();
    let stale = Duration::hours(cfg.stale_activity_timeout_hours as i64);
    let grace = Duration::minutes(cfg.missed_heartbeat_grace_minutes as i64);

    let mut classified: HashMap<String, Vec<AlertType>> = HashMap::new();
    let mut activity_map: HashMap<String, AgentActivity> = HashMap::new();
    let mut known_agents: HashSet<String> = HashSet::new();

    for a in &activity {
        known_agents.insert(a.agent_id.clone());
        let alerts = classify(a, now, stale, grace);
        if !alerts.is_empty() {
            classified.insert(a.agent_id.clone(), alerts);
        }
        activity_map.insert(a.agent_id.clone(), a.clone());
    }

    let (fires, recovers) = reconcile(classified, &activity_map, &known_agents, state, now);

    // Reuse existing "down"/"recovery" event types so the UI's existing
    // ALL_ALERT_EVENTS toggle covers these without UI changes. The body
    // text disambiguates inactivity vs service-down.
    for fire in fires {
        let msg = format_fire_message(&fire);
        alerter.send(alert_config, &msg, "down").await;
    }
    for rec in recovers {
        let msg = format_recover_message(&rec);
        alerter.send(alert_config, &msg, "recovery").await;
    }

    Ok(())
}

fn format_fire_message(f: &Fire) -> String {
    match f.alert_type {
        AlertType::StaleActivity => {
            let last = f.latest_activity_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".to_string());
            format!("agent {} inactive (last activity: {})", f.agent_id, last)
        }
        AlertType::MissedHeartbeat => {
            let expected = f.next_expected_heartbeat_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "?".to_string());
            format!("agent {} missed heartbeat (expected at {})", f.agent_id, expected)
        }
    }
}

fn format_recover_message(r: &Recover) -> String {
    let kind = match r.alert_type {
        AlertType::StaleActivity => "activity",
        AlertType::MissedHeartbeat => "heartbeat",
    };
    format!("agent {} recovered ({})", r.agent_id, kind)
}
```

The `Alerter::send(&self, config: &AlertConfig, message: &str, event_type: &str)` method already exists at [alerter.rs:106](crates/opex-watchdog/src/alerter.rs#L106). It filters by `config.events.contains(event_type)`, so reusing `"down"`/`"recovery"` (already in the default `AlertConfig.events`) means operators get inactivity alerts without UI/config changes. The body text (`"agent X inactive (last activity: ...)"`) disambiguates from real service-down alerts.

`AlertConfig` is fetched once at watchdog startup (via `alerter.fetch_config()`) and stored in a local variable `alert_config` in `main.rs`; it's hot-reloaded by the existing config refresh logic. The new `tick` takes `&alert_config` as an explicit parameter — keeps the dependency flow visible.

- [ ] **Step 2: Verify wiremock is a dev-dep, add if missing**

```bash
grep -n "wiremock" crates/opex-watchdog/Cargo.toml
```

If absent, add to `[dev-dependencies]`:

```toml
[dev-dependencies]
wiremock = "0.6"
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 3: Write the integration test**

Create `crates/opex-watchdog/tests/integration_inactivity.rs`:

```rust
//! Integration: watchdog inactivity::tick against a wiremock-mocked
//! core endpoint. Drives `tick` directly — all referenced types and
//! functions are `pub` in inactivity.rs (Task 3 Step 1).
//!
//! For this to compile, the integration test must be able to import
//! from the watchdog crate. The watchdog crate is `[[bin]]` only; to
//! make its modules visible to integration tests, the crate must
//! expose a thin `src/lib.rs` that re-exports the needed modules.
//! This is the standard Rust pattern for binary crates with
//! integration tests — see [the Rust book chapter on integration tests
//! for binary crates].
//!
//! Step 1 of this test setup, if not already done in Task 3: create
//! `crates/opex-watchdog/src/lib.rs` with:
//!
//! ```rust
//! pub mod alerter;
//! pub mod config;
//! pub mod inactivity;
//! ```
//!
//! and update `Cargo.toml` to declare both the lib and the bin (the
//! bin already has `[[bin]] name = "opex-watchdog" path = "src/main.rs"`;
//! add `[lib] name = "opex_watchdog" path = "src/lib.rs"`).
//! `main.rs` then imports its modules via `use opex_watchdog::...`
//! OR keeps `mod alerter; mod config; mod inactivity;` — both work.
//!
//! Simpler alternative if lib re-export feels heavy: relocate this
//! integration test INTO `inactivity.rs` as a `#[tokio::test]` inside
//! the existing `#[cfg(test)] mod tests` block. Same coverage,
//! single-file diff. Pick whichever fits the codebase style.

use std::collections::HashMap;

use opex_watchdog::alerter::{AlertConfig, Alerter};
use opex_watchdog::config::WatchdogSettings;
use opex_watchdog::inactivity::{self, EpisodeKey, AlertState};

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn minimal_settings() -> WatchdogSettings {
    // Construct via default + override; assumes `WatchdogSettings: Default`
    // or has a builder. If it doesn't, the implementer adds a `Default`
    // impl during Task 3 (trivial — all numeric defaults).
    WatchdogSettings {
        enabled: true,
        interval_secs: 60,
        cooldown_secs: 300,
        grace_period_secs: 60,
        flap_window_secs: 600,
        flap_threshold: 3,
        session_retry_enabled: true,
        session_retry_stale_secs: 90,
        session_retry_max_attempts: 3,
        stale_activity_timeout_hours: 6,
        missed_heartbeat_grace_minutes: 10,
    }
}

#[tokio::test]
async fn tick_fires_alert_for_stale_agent() {
    let mock_server = MockServer::start().await;

    let very_old = chrono::Utc::now() - chrono::Duration::hours(10);
    Mock::given(method("GET"))
        .and(path("/api/watchdog/agent-activity"))
        .and(header("Authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "agent_id": "Hyde",
                "latest_activity_at": very_old.to_rfc3339(),
                "next_expected_heartbeat_at": null
            }
        ])))
        .mount(&mock_server)
        .await;

    // POST /api/channels/notify → expect exactly 1 hit (one fire alert).
    Mock::given(method("POST"))
        .and(path("/api/channels/notify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(1)
        .named("notify-on-fire")
        .mount(&mock_server)
        .await;

    let http = reqwest::Client::new();
    let alerter = Alerter::new(&mock_server.uri(), "test-token");
    let alert_config = AlertConfig {
        channel_ids: vec!["test-channel-uuid".to_string()],
        events: vec!["down".into(), "recovery".into()],
    };
    let cfg = minimal_settings();
    let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();

    inactivity::tick(&http, &mock_server.uri(), "test-token", &cfg, &mut state, &alerter, &alert_config)
        .await
        .expect("tick must succeed against the mock");

    // wiremock asserts .expect(1) on Mock drop at end-of-test scope.
    assert_eq!(state.len(), 1, "one episode should be open after fire");
}

#[tokio::test]
async fn tick_emits_recovery_when_agent_returns() {
    let mock_server = MockServer::start().await;

    let fresh = chrono::Utc::now();
    Mock::given(method("GET"))
        .and(path("/api/watchdog/agent-activity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "agent_id": "Hyde", "latest_activity_at": fresh.to_rfc3339(), "next_expected_heartbeat_at": null }
        ])))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/channels/notify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(1)
        .named("notify-on-recovery")
        .mount(&mock_server)
        .await;

    let http = reqwest::Client::new();
    let alerter = Alerter::new(&mock_server.uri(), "test-token");
    let alert_config = AlertConfig {
        channel_ids: vec!["test-channel-uuid".to_string()],
        events: vec!["down".into(), "recovery".into()],
    };
    let cfg = minimal_settings();

    // Pre-seed an open episode for Hyde to simulate "was inactive, now recovered".
    let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
    state.insert(
        ("Hyde".to_string(), inactivity::AlertType::StaleActivity),
        AlertState { fired_at: chrono::Utc::now() - chrono::Duration::hours(1) },
    );

    inactivity::tick(&http, &mock_server.uri(), "test-token", &cfg, &mut state, &alerter, &alert_config)
        .await
        .expect("tick must succeed");

    assert!(state.is_empty(), "episode must be cleared after recovery");
}

#[tokio::test]
async fn tick_tolerates_endpoint_500() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/watchdog/agent-activity"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let http = reqwest::Client::new();
    let alerter = Alerter::new(&mock_server.uri(), "test-token");
    let alert_config = AlertConfig::default();
    let cfg = minimal_settings();
    let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();

    let result = inactivity::tick(&http, &mock_server.uri(), "test-token", &cfg, &mut state, &alerter, &alert_config).await;
    assert!(result.is_err(), "tick returns Err on endpoint failure (logged + swallowed by main.rs loop)");
    assert!(state.is_empty(), "no episodes opened on error path");
}
```

> **Implementer setup note:** if `crates/opex-watchdog/` doesn't yet have `src/lib.rs`, add it during Task 3 Step 1 with the three `pub mod` lines above and add a `[lib]` section to `Cargo.toml` pointing at it. The single-file diff is trivial and unlocks integration tests cleanly. The existing `main.rs` continues to `mod alerter; mod config; mod inactivity;` — those work alongside the lib (Rust allows a crate to be both `[lib]` and `[[bin]]`).

- [ ] **Step 4: Run the unit tests + integration test, confirm they pass**

```bash
cargo test -p opex-watchdog -- --nocapture
```

Expected: previous 9 unit tests (one of which is `reconcile_silent_cleanup_on_disappeared_agent`) + the new integration test PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-watchdog/
git commit -m "$(cat <<'EOF'
feat(watchdog): inactivity::tick HTTP fetch + alert dispatch

Wires the pure classify/reconcile logic against the new core endpoint
and the existing Alerter. fetch_agent_activity does the Bearer-token
GET; tick orchestrates classify → reconcile → fire/recover alerts.

Integration test uses wiremock to mock both ends (the agent-activity
endpoint and the notify endpoint) and asserts the expected number of
POST /api/channels/notify calls.
EOF
)"
```

---

## Task 5: Watchdog config + main.rs loop wiring

**Files:**

- Modify: `crates/opex-watchdog/src/config.rs`
- Modify: `crates/opex-watchdog/src/main.rs`

- [ ] **Step 1: Add config fields**

Edit `crates/opex-watchdog/src/config.rs`. Find the `WatchdogSettings` struct (around line 13) and add two fields with `#[serde(default = "...")]` attributes:

```rust
    #[serde(default = "default_stale_activity_timeout_hours")]
    pub stale_activity_timeout_hours: u64,

    #[serde(default = "default_missed_heartbeat_grace_minutes")]
    pub missed_heartbeat_grace_minutes: u64,
```

And add the default helpers near the other `default_*` helpers (around line 70):

```rust
fn default_stale_activity_timeout_hours() -> u64 { 6 }
fn default_missed_heartbeat_grace_minutes() -> u64 { 10 }
```

Update the existing `parse_minimal_config` test (around line 100) to assert the new defaults:

```rust
        assert_eq!(cfg.watchdog.stale_activity_timeout_hours, 6);
        assert_eq!(cfg.watchdog.missed_heartbeat_grace_minutes, 10);
```

- [ ] **Step 2: Allocate state + call tick in `main.rs`**

Edit `crates/opex-watchdog/src/main.rs`. Find the block initialising state maps (around line 55 where `was_down`, `was_resource_warning` are declared) and add:

```rust
    let mut inactivity_state: HashMap<inactivity::EpisodeKey, inactivity::AlertState> = HashMap::new();
```

Inside the main `loop` (around line 75+), after the existing `resources::check_resources(...)` call but before `tokio::time::sleep(...)`, add:

```rust
        if let Err(e) = inactivity::tick(
            &http,
            &core_url,
            &auth_token,
            &cfg.watchdog,
            &mut inactivity_state,
            &alerter,
            &alert_config,
        ).await {
            tracing::warn!(error = %e, "inactivity tick failed");
        }
```

`alert_config` already exists as a local variable in `main.rs` — it's the same `AlertConfig` value that the existing `resources::check_resources` and the resource-warning blocks already receive (search for `alerter.send(&alert_config, ...)` in `main.rs` for the pattern).

- [ ] **Step 3: Run config tests**

```bash
cargo test -p opex-watchdog --bin opex-watchdog config -- --nocapture
```

Expected: existing config tests pass with the two new assertions.

- [ ] **Step 4: Run the full build**

```bash
cargo build --workspace --all-targets
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-watchdog/src/config.rs crates/opex-watchdog/src/main.rs
git commit -m "$(cat <<'EOF'
feat(watchdog): wire inactivity::tick into main loop + add config

Two new [watchdog] config fields with sensible defaults:
- stale_activity_timeout_hours = 6
- missed_heartbeat_grace_minutes = 10

Loop integration: inactivity_state HashMap lives alongside the
existing was_down/was_resource_warning maps. tick() failure is logged
and swallowed — never crashes the watchdog process.
EOF
)"
```

---

## Task 6: Promote `ToolExecutionContext` from `#[cfg(test)]` to production

Drops the `#[cfg(test)]` gates, swaps `Mutex<HashMap>` for `DashMap`, adds `max_entries` + batch eviction. No callers wired yet — only the struct surface changes.

**Files:**

- Modify: `crates/opex-core/src/tools/yaml_tools.rs`

- [ ] **Step 1: Update the test that validates current behaviour**

In `crates/opex-core/src/tools/yaml_tools.rs` find the `mod tests` block and locate the existing `execution_context_cache_basic` test. Add three new tests next to it (these will fail until the production code is written):

```rust
    #[tokio::test]
    async fn cache_key_object_keys_are_order_independent() {
        let a = build_cache_key(
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"a": 1, "b": 2}),
            &[],
        );
        let b = build_cache_key(
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"b": 2, "a": 1}),
            &[],
        );
        assert_eq!(a, b, "object key order must not matter (serde_json::Map is BTreeMap)");
    }

    #[tokio::test]
    async fn cache_key_array_order_matters() {
        let a = build_cache_key(
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"tags": ["a", "b"]}),
            &[],
        );
        let b = build_cache_key(
            "x",
            "POST",
            "https://api.test/v",
            &serde_json::json!({"tags": ["b", "a"]}),
            &[],
        );
        assert_ne!(a, b, "array element order is part of the cache key");
    }

    #[tokio::test]
    async fn cache_evicts_oldest_at_cap_with_min_one() {
        // max_entries = 3 → eviction target = max(3/10, 1) = 1 per write.
        let ctx = ToolExecutionContext::new(3);
        ctx.set_cached("k1", "v1", 60).await;
        ctx.set_cached("k2", "v2", 60).await;
        ctx.set_cached("k3", "v3", 60).await;
        assert_eq!(ctx.cache_len(), 3);
        ctx.set_cached("k4", "v4", 60).await;
        // We expect the cache to NOT exceed 3 (at cap, one evicted before insert).
        assert!(ctx.cache_len() <= 3, "soft cap must hold at max_entries");
        assert!(ctx.get_cached("k4").await.is_some(), "newest write must be present");
    }
```

- [ ] **Step 2: Run tests to confirm they fail to compile**

```bash
cargo test -p opex-core --lib tools::yaml_tools -- --nocapture
```

Expected: FAIL (compile error) — `ToolExecutionContext::new` doesn't take a number, `cache_len()` doesn't exist, etc.

- [ ] **Step 3: Promote the cache types and methods**

In `crates/opex-core/src/tools/yaml_tools.rs`, find the `#[cfg(test)]` blocks that wrap `CachedResponse`, `ToolExecutionContext`, the `impl ToolExecutionContext`, `build_cache_key` etc. (currently around lines 180–250 after the previous refactor — verify with grep). Replace them with production versions:

Remove the `#[cfg(test)]` line above `struct CachedResponse`:

```rust
// Drop this line:  #[cfg(test)]
pub(crate) struct CachedResponse {
    body: String,
    expires_at: std::time::Instant,
}
```

Replace the entire `ToolExecutionContext` block with:

```rust
/// Shared response cache for YAML tools. Process-wide singleton held inside
/// `Arc<ToolExecutionContext>` on `AgentConfig`. Lazy TTL on read, batch
/// eviction on write at the soft cap.
pub struct ToolExecutionContext {
    cache: dashmap::DashMap<String, CachedResponse>,
    max_entries: usize,
}

impl ToolExecutionContext {
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: dashmap::DashMap::new(),
            max_entries,
        }
    }

    /// Test-only inspection.
    #[cfg(test)]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    pub async fn get_cached(&self, key: &str) -> Option<String> {
        let now = std::time::Instant::now();
        // Read first.
        let body = {
            let entry = self.cache.get(key)?;
            if now >= entry.expires_at {
                None
            } else {
                Some(entry.body.clone())
            }
        };
        if body.is_none() {
            // Expired — drop the entry.
            self.cache.remove(key);
        }
        body
    }

    pub async fn set_cached(&self, key: &str, body: &str, ttl_secs: u64) {
        if self.cache.len() >= self.max_entries {
            let target_remove = (self.max_entries / 10).max(1);
            let mut victims: Vec<(String, std::time::Instant)> = self.cache
                .iter()
                .map(|e| (e.key().clone(), e.value().expires_at))
                .collect();
            victims.sort_by_key(|(_, exp)| *exp);
            for (k, _) in victims.into_iter().take(target_remove) {
                self.cache.remove(&k);
            }
        }
        self.cache.insert(
            key.to_string(),
            CachedResponse {
                body: body.to_string(),
                expires_at: std::time::Instant::now() + std::time::Duration::from_secs(ttl_secs),
            },
        );
    }
}
```

Remove `#[cfg(test)]` from `fn build_cache_key(...)` and make it `pub(crate)`. Update its signature to take `method` and `endpoint`:

```rust
pub(crate) fn build_cache_key(
    tool_name: &str,
    method: &str,
    endpoint: &str,
    params: &serde_json::Value,
    key_params: &[String],
) -> String {
    let mut key = format!("{tool_name}|{method}|{endpoint}|");
    if let Some(obj) = params.as_object() {
        if key_params.is_empty() {
            // All params in sorted order (BTreeMap iteration already sorted).
            for (k, v) in obj {
                key.push_str(k);
                key.push('=');
                key.push_str(&v.to_string());
                key.push('&');
            }
        } else {
            for kp in key_params {
                if let Some(v) = obj.get(kp) {
                    key.push_str(kp);
                    key.push('=');
                    key.push_str(&v.to_string());
                    key.push('&');
                }
            }
        }
    }
    key
}
```

The existing `execution_context_cache_basic` test was written against the old `ToolExecutionContext::new()` (no-arg). Update its call to `ToolExecutionContext::new(1000)` so it still passes.

- [ ] **Step 4: Run the tests, confirm they pass**

```bash
cargo test -p opex-core --lib tools::yaml_tools -- --nocapture
```

Expected: PASS — basic + three new tests.

- [ ] **Step 5: Run the workspace build**

```bash
cargo build --workspace --all-targets
```

Expected: PASS. No other call sites depend on these symbols yet (they were `#[cfg(test)]`), so this should compile cleanly.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/tools/yaml_tools.rs
git commit -m "$(cat <<'EOF'
feat(tools): promote YAML cache infrastructure to production

ToolExecutionContext, CachedResponse, build_cache_key — all out of
#[cfg(test)] and into production. Internal cache map is now DashMap
(concurrent reads, no lock contention) with soft-cap eviction:

- new(max_entries): pre-size the cap
- get_cached: lazy TTL — expired entries removed on read
- set_cached: at cap, evict the (max_entries/10).max(1) oldest by
  expires_at before inserting

Cache key format: {tool_name}|{method}|{endpoint}|sorted_params.
Tests pin both order-independence on object keys (BTreeMap) and
order-dependence on array elements (Vec preserves order).

No callers yet — that's the next commit.
EOF
)"
```

---

## Task 7: Thread `Arc<ToolExecutionContext>` through `AgentConfig`

**Files:**

- Modify: `crates/opex-core/src/agent/agent_config.rs`
- Modify: `crates/opex-core/src/config/mod.rs` (add `ToolCacheConfig`)
- Modify: `crates/opex-core/src/gateway/handlers/agents/lifecycle.rs` (pass field)
- Modify: `crates/opex-core/src/main.rs` (construct once at startup)

- [ ] **Step 1: Add `ToolCacheConfig` to `config/mod.rs`**

Find the existing `[tools.cache]`-like sibling — the file has `CleanupConfig`, `WatchdogConfig`, etc. Add (placed near other tool-related config sections):

```rust
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ToolCacheConfig {
    /// Maximum entries in the YAML-tool response cache. Soft cap — at the
    /// limit, ~10 % of oldest entries (min 1) are evicted before insert.
    #[serde(default = "default_tool_cache_max_entries")]
    pub max_entries: usize,
}

fn default_tool_cache_max_entries() -> usize { 1000 }

impl Default for ToolCacheConfig {
    fn default() -> Self {
        Self { max_entries: default_tool_cache_max_entries() }
    }
}
```

And nest it under `AppConfig` as part of a `ToolsConfig` (which may need to be introduced too — verify the file structure first). If `AppConfig` doesn't have a `tools` section yet, the simplest path is to add a new top-level field:

```rust
    /// YAML-tool response cache tuning.
    #[serde(default)]
    pub tools_cache: ToolCacheConfig,
```

(If a `[tools]` section exists already, nest it under `pub tools: ToolsConfig` with `cache: ToolCacheConfig`. Pick the smaller diff during implementation.)

- [ ] **Step 2: Add the field on `AgentConfig`**

Edit `crates/opex-core/src/agent/agent_config.rs`. Near the `pub metrics: Arc<crate::metrics::MetricsRegistry>,` field (around line 56), add:

```rust
    /// Shared YAML-tool response cache (process-wide).
    pub tool_exec_ctx: Arc<crate::tools::yaml_tools::ToolExecutionContext>,
```

- [ ] **Step 3: Add `tool_exec_ctx` field to `AgentDeps`**

Edit `crates/opex-core/src/gateway/state.rs`. Find the `pub struct AgentDeps { ... }` block (around line 53) and add:

```rust
    /// Shared YAML-tool response cache (process-wide singleton).
    pub tool_exec_ctx: std::sync::Arc<crate::tools::yaml_tools::ToolExecutionContext>,
```

`AgentDeps` is the natural home — it's already "shared deps needed to spawn agents at runtime" and holds analogous shared resources (`tool_embed_cache`, `penalty_cache`, `audit_queue`). The cache is a per-agent dep, not infrastructure, so it doesn't belong on `InfraServices` (which holds `db`, `embedder`, etc.).

Update the test-only `AgentDeps::for_test()` / similar constructor (around line 68+ — search for `#[cfg(test)] impl AgentDeps`) to include the new field. Use `Arc::new(ToolExecutionContext::new(100))` — small cap fine for tests.

- [ ] **Step 4: Construct the cache once at startup and thread into `AgentDeps`**

Edit `crates/opex-core/src/main.rs`. Find the place where `AgentDeps { ... }` is constructed (search for `AgentDeps {` — should be one or two call sites). Add construction:

```rust
    let tool_exec_ctx = std::sync::Arc::new(
        crate::tools::yaml_tools::ToolExecutionContext::new(
            cfg.config.tools_cache.max_entries,
        ),
    );
```

placed before the `AgentDeps { ... }` literal. Then add to the literal:

```rust
        tool_exec_ctx: tool_exec_ctx.clone(),
```

(The `cfg.config.tools_cache` path assumes Task 7 Step 1 added the field as a top-level on `AppConfig`. If the implementer chose to nest under `[tools].cache` instead, the path is `cfg.config.tools.cache.max_entries` — keep consistent with the Step 1 choice.)

- [ ] **Step 5: Pass the field at `AgentConfig` construction**

Edit `crates/opex-core/src/gateway/handlers/agents/lifecycle.rs:152`. The `AgentConfig { ... }` block needs:

```rust
        tool_exec_ctx: deps.tool_exec_ctx.clone(),
```

added among the other `*.clone()` lines (e.g. just after `metrics: infra.metrics.clone(),`).

- [ ] **Step 6: Run the build, fix any tests that construct `AgentConfig` directly**

```bash
cargo build --workspace --all-targets
```

If `AgentConfig` is built in test fixtures or unit tests, they need the new field too. The compiler will name each location — for each one, add:

```rust
            tool_exec_ctx: std::sync::Arc::new(
                crate::tools::yaml_tools::ToolExecutionContext::new(100),
            ),
```

(100 is fine for tests — small but non-zero.)

- [ ] **Step 7: Run the full tests**

```bash
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  cargo test --workspace --lib 2>&1 | grep -E "test result|FAILED"
```

Expected: no failures attributable to the new field.

- [ ] **Step 8: Commit**

```bash
git add crates/opex-core/src/agent/agent_config.rs \
        crates/opex-core/src/config/mod.rs \
        crates/opex-core/src/gateway/handlers/agents/lifecycle.rs \
        crates/opex-core/src/gateway/state.rs \
        crates/opex-core/src/main.rs
git commit -m "$(cat <<'EOF'
feat(config): thread Arc<ToolExecutionContext> through AgentConfig

Process-wide singleton ToolExecutionContext constructed once in main.rs
from [tools.cache] config, shared across all agents via Arc clone in
each AgentConfig (alongside the existing Arc<MetricsRegistry>).

engine_dispatch will reach it via self.cfg().tool_exec_ctx in the
next commit. No behaviour change yet — the cache is allocated but
unused.
EOF
)"
```

---

## Task 8: Wire cache into `engine_dispatch.rs` YAML-tool path

**Files:**

- Modify: `crates/opex-core/src/agent/engine_dispatch.rs`
- Create: `crates/opex-core/tests/integration_yaml_cache.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/opex-core/tests/integration_yaml_cache.rs`:

```rust
//! Integration: YAML tool response cache hit/miss against wiremock.

mod support;

use support::TestHarness;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cache_hit_skips_http_call() {
    let mock_server = MockServer::start().await;

    // Mock expects EXACTLY ONE call — second invocation must hit cache.
    Mock::given(method("GET"))
        .and(path("/v"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": "v1"})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let harness = TestHarness::with_yaml_tool(
        "search",
        &mock_server.uri(),
        Some(60), // ttl_secs
    )
    .await;

    let r1 = harness.invoke_tool("search", serde_json::json!({"q": "hello"})).await;
    let r2 = harness.invoke_tool("search", serde_json::json!({"q": "hello"})).await;

    assert_eq!(r1, r2, "second call must return same body from cache");
    // wiremock asserts on Mock drop: 1 call made.
}

#[tokio::test]
async fn cache_miss_on_distinct_args() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": "v1"})))
        .expect(2)
        .mount(&mock_server)
        .await;

    let harness = TestHarness::with_yaml_tool("search", &mock_server.uri(), Some(60)).await;
    let _ = harness.invoke_tool("search", serde_json::json!({"q": "a"})).await;
    let _ = harness.invoke_tool("search", serde_json::json!({"q": "b"})).await;
}

#[tokio::test]
async fn non_2xx_response_not_cached() {
    let mock_server = MockServer::start().await;
    // First call: 500. Second call: 200. If 500 were cached, the second
    // wouldn't reach the mock — the .expect(2) would fail.
    Mock::given(method("GET"))
        .and(path("/v"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": "ok"})))
        .expect(1)
        .mount(&mock_server)
        .await;

    let harness = TestHarness::with_yaml_tool("search", &mock_server.uri(), Some(60)).await;
    let _ = harness.invoke_tool("search", serde_json::json!({"q": "x"})).await;
    let r2 = harness.invoke_tool("search", serde_json::json!({"q": "x"})).await;
    assert!(r2.contains("ok"), "second call must hit the 200 branch, not cached 500");
}

#[tokio::test]
async fn channel_action_bypasses_cache() {
    // Tools with `channel_action: {...}` route binary output to a channel;
    // their HTTP response is never returned as text to the LLM. Caching
    // such a response is meaningless. The dispatch path must skip cache
    // logic when channel_action is configured even if `cache:` is set.
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xFF; 100]))
        .expect(2) // both calls hit HTTP — cache bypassed
        .mount(&mock_server)
        .await;

    let harness = TestHarness::with_yaml_tool_channel_action(
        "send_voice",
        &mock_server.uri(),
        Some(60), // cache.ttl = 60 — should still be bypassed
    )
    .await;
    let _ = harness.invoke_tool("send_voice", serde_json::json!({"text": "hi"})).await;
    let _ = harness.invoke_tool("send_voice", serde_json::json!({"text": "hi"})).await;
}

#[tokio::test]
async fn pagination_bypasses_cache() {
    // Tools with `pagination: {...}` auto-fetch multiple pages mid-execution.
    // Responses are not idempotent without full pagination state. The
    // dispatch path must skip cache logic when pagination is configured
    // even if `cache:` is set.
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})))
        .expect(2)
        .mount(&mock_server)
        .await;

    let harness = TestHarness::with_yaml_tool_paginated(
        "list_items",
        &mock_server.uri(),
        Some(60),
    )
    .await;
    let _ = harness.invoke_tool("list_items", serde_json::json!({})).await;
    let _ = harness.invoke_tool("list_items", serde_json::json!({})).await;
}
```

> **Implementer note:** the test helpers `with_yaml_tool_channel_action` and `with_yaml_tool_paginated` are minor variants of `with_yaml_tool` — they build the same `YamlToolDef` plus `channel_action: Some(...)` / `pagination: Some(...)` respectively. Add them next to `with_yaml_tool` in `tests/support/`. If `TestHarness` doesn't exist yet, build the minimal version inside `tests/support/mod.rs` borrowing the AppState construction pattern from `tests/integration_session_timeline_cleanup.rs`.

(If `TestHarness` doesn't have `with_yaml_tool` / `invoke_tool` helpers, look at how `tests/integration_session_timeline_cleanup.rs` builds its harness for a similar tool-flow test. Add the missing helpers to `tests/support/` — they should be small enough to inline if needed.)

- [ ] **Step 2: Run the tests to confirm they fail**

```bash
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  cargo test -p opex-core --test integration_yaml_cache -- --nocapture
```

Expected: FAIL — `cache_hit_skips_http_call` will show wiremock expected 1 call but got 2.

- [ ] **Step 3: Wire cache lookup into `execute_tool_call_inner`**

Edit `crates/opex-core/src/agent/engine_dispatch.rs`. Inside `execute_tool_call_inner`, find the YAML tool path (around lines 169–215 where `find_yaml_tool` resolves and the HTTP client is selected). Just BEFORE the `client = ...` selection (around line 213), add:

```rust
                // YAML tool cache — pre-execution lookup.
                let cache_key = match &yaml_tool.cache {
                    Some(cfg) if yaml_tool.channel_action.is_none() && yaml_tool.pagination.is_none() => {
                        Some(crate::tools::yaml_tools::build_cache_key(
                            &yaml_tool.name,
                            &yaml_tool.method,
                            &yaml_tool.endpoint,
                            arguments,
                            &cfg.key_params,
                        ))
                    }
                    _ => None,
                };

                if let Some(ref key) = cache_key {
                    if let Some(body) = self.cfg().tool_exec_ctx.get_cached(key).await {
                        tracing::debug!(tool = %yaml_tool.name, "yaml tool cache hit");
                        return Ok(body);
                    }
                }
```

After the HTTP execution returns (you'll see the `yaml_tool.execute_oauth(...)` or equivalent call followed by handling of the result), gate the cache write on success. Find the `Ok(body) => { ... }` arm and add at its end (only on 2xx):

```rust
                if let (Some(ref key), Some(ref cfg)) =
                    (cache_key.as_ref(), yaml_tool.cache.as_ref())
                {
                    self.cfg().tool_exec_ctx.set_cached(key, &body, cfg.ttl).await;
                }
```

(The exact location of "the success arm" depends on how `execute_oauth` returns errors vs. success — verify by reading the surrounding code. The principle: cache only on the success path, never on `Err(_)`.)

- [ ] **Step 4: Run the tests, confirm they pass**

```bash
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  cargo test -p opex-core --test integration_yaml_cache -- --nocapture
```

Expected: all three tests PASS.

- [ ] **Step 5: Run the full workspace tests**

```bash
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  cargo test --workspace 2>&1 | grep -E "test result|FAILED"
```

Expected: no new failures.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/engine_dispatch.rs \
        crates/opex-core/tests/integration_yaml_cache.rs
git commit -m "$(cat <<'EOF'
feat(tools): wire YAML response cache into engine_dispatch

When a YAML tool has `cache: { ttl: N }` configured AND has no
channel_action AND no pagination, execute_tool_call_inner:
  1. Computes the cache key (tool|method|endpoint|sorted-params).
  2. Checks the process-wide ToolExecutionContext for an unexpired
     hit — if found, returns it without HTTP.
  3. On HTTP success (2xx), stores the response body.

Errors (non-2xx, network failures) are never cached.

Integration tests via wiremock verify:
- second identical call hits cache (mock receives 1 request)
- distinct args bypass cache (mock receives 2)
- non-2xx responses don't poison the cache
EOF
)"
```

---

## Task 9: Acceptance verification (no commit unless defect found)

Verification-only task.

- [ ] **Step 1: All AC #1–#7 for Part A**

```bash
echo "AC1 — endpoint returns 200 with valid token:"
curl -sf -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  http://localhost:18789/api/watchdog/agent-activity | python3 -m json.tool | head -20

echo "AC2 — without token, expect 401:"
curl -s -o /dev/null -w "%{http_code}\n" http://localhost:18789/api/watchdog/agent-activity
```

Expected: AC1 prints a JSON array; AC2 prints `401`.

- [ ] **Step 2: Watchdog tick logs no error**

```bash
# Tail the watchdog log for one tick interval.
make logs 2>&1 | grep -E "inactivity tick|stale_activity|missed_heartbeat" | head -5
```

Expected: either no output (no current alerts) or fire/recover lines — never "inactivity tick failed".

- [ ] **Step 3: Force an alert by aging a session**

```bash
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  psql -c "UPDATE sessions SET activity_at = NOW() - INTERVAL '7 hours', last_message_at = NOW() - INTERVAL '7 hours' WHERE agent_id = (SELECT agent_id FROM sessions LIMIT 1);"
```

Wait one watchdog interval (default 60 s). Expect exactly one alert message in the configured channel.

- [ ] **Step 4: All AC #1–#7 for Part B**

Manually craft a YAML tool with `cache: { ttl: 300 }` and invoke it twice via `cargo run --bin opex-core` against a public API. Tail the core log:

```bash
make logs 2>&1 | grep "yaml tool cache hit"
```

Expected: one cache-hit line for the second invocation.

- [ ] **Step 5: Final workspace build + tests**

```bash
cargo build --workspace --all-targets
DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test \
  cargo test --workspace 2>&1 | grep -E "test result"
```

Expected: PASS for all suites.

- [ ] **Step 6: No commit needed — verification-only task**

If anything fails, fix it in a follow-up commit referencing the specific AC.

---

## Self-review

**Spec coverage check** — every Part of the spec mapped to tasks:

- §A.1 (architecture): the HTTP-only choice is preserved by Tasks 2–5 (no DB driver added to watchdog).
- §A.2 (endpoint shape): Task 2.
- §A.3 (config): Task 5 (config fields).
- §A.4 (module surface): Task 3 (types + pure functions) + Task 4 (HTTP + tick).
- §A.5 (classification logic): Task 3 (classify + unit tests).
- §A.6 (episode dedup + silent cleanup): Task 3 (reconcile + the disappeared-agent test).
- §A.7 (loop integration): Task 5.
- §B.1 (cache activation): Task 6.
- §B.2 (shared placement on AgentConfig): Task 7.
- §B.3 (config): Task 7 (`[tools.cache]` section).
- §B.4 (cache key): Task 6 (build_cache_key + order-related tests).
- §B.5 (dispatch integration): Task 8.
- §B.6 (eviction): Task 6 (the eviction test + the `(max_entries/10).max(1)` line).
- §B.7 (lazy TTL): Task 6 (get_cached drops expired entries).

**Type-consistency check** — names and shapes match across tasks:

- `AlertType` enum: defined in Task 3, referenced verbatim in Task 4 (`inactivity::EpisodeKey`) and Task 5 (HashMap allocation).
- `AgentActivity` struct: defined in Task 3 with `latest_activity_at` / `next_expected_heartbeat_at`; Task 2 (handler) emits the same field names.
- `ToolExecutionContext::new(max_entries)`: Task 6 defines this signature; Task 7 calls it with a `usize` from config.
- `self.cfg().tool_exec_ctx`: Task 7 adds the field; Task 8 reads it.
- `build_cache_key(tool_name, method, endpoint, params, key_params)`: Task 6 declares this signature; Task 8 calls with exactly these args in order.

**Placeholder scan** — none of the "TBD / implement later / Add appropriate error handling" patterns appear. Two places use deliberate `implementer note` wording for genuine ambiguity (visibility decision in Task 4 Step 3, `tools.cache` nesting in Task 7 Step 1) — each gives a concrete recommendation rather than punting.

---

## Out-of-scope reminders (from spec)

These are NOT in this plan and must NOT creep into the implementation:

- Per-agent inactivity thresholds (Hyde 1 h, Alma 24 h).
- `scheduled_jobs` monitoring as a separate signal.
- Cron-cadence-derived auto-grace.
- Watchdog DB pool.
- Per-agent cache isolation.
- Cache manual-invalidation API.
- Metrics export for cache hit/miss.
- Background cache sweeper.
- Negative caching (cache 4xx).
