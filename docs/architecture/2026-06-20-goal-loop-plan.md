# `/goal` autonomous loop — Implementation Plan (Phase 2b)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A channel user sets `/goal <text>`; the agent works autonomously turn-after-turn until an auxiliary judge model declares the goal done, a turn budget is hit, or the user pauses/clears/preempts it.

**Architecture:** DB-backed `session_goals`; a per-session background goal-driver task (mirrors `SessionAgentPool::spawn_live_agent`) that calls a new `run_goal_turn` engine entry (mirrors `handle_isolated_via_pipeline` but continues the existing session), delivers each turn via `channel_router`, and judges via the compaction/aux provider. A per-session goal lock serializes the driver against real user turns.

**Tech Stack:** Rust 2024 (sqlx, tokio, dashmap), PostgreSQL 17, Next.js (one chat-store event handler).

## Global Constraints

- rustls-only; `cargo clippy --bin opex-core --all-targets -- -D warnings` clean.
- Rust app-tree tests run under `cargo test --bin opex-core`. DB tests use the test postgres + `#[sqlx::test(migrations = "../../migrations")]`.
- Migrations runtime-loaded; `make remote-deploy` syncs them.
- Conventional commits, no `Co-Authored-By`. No `git push` unless asked.
- **Integration tasks (4–8) note:** where a step says "mirror `X` (file:line)", read that code at implementation time and reconcile exact field/fn names; the surrounding contract (signatures in the Interfaces block) is fixed.

---

### Task 1: `session_goals` table + storage

**Files:**
- Create: `migrations/056_session_goals.sql`
- Create: `crates/opex-core/src/db/session_goals.rs`
- Modify: `crates/opex-core/src/db/mod.rs` (add `pub mod session_goals;` in the "Remaining modules" group)

**Interfaces:**
- Produces: `pub struct GoalRow { pub session_id: Uuid, pub goal_text: String, pub status: String, pub turn_count: i32, pub max_turns: i32, pub subgoals: Vec<String>, pub last_verdict: Option<String>, pub consecutive_judge_failures: i32 }` with `impl GoalRow { pub fn is_running(&self) -> bool; pub fn budget_left(&self) -> bool }`.
- Produces: `get`, `upsert`, `set_status`, `bump_turn`, `set_subgoals`, `record_verdict`, `clear` (signatures in Step 5).

- [ ] **Step 1: Create migration** `migrations/056_session_goals.sql`:

```sql
CREATE TABLE session_goals (
    session_id   UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    goal_text    TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'active'
                 CHECK (status IN ('active', 'paused', 'done', 'cleared')),
    turn_count   INT  NOT NULL DEFAULT 0,
    max_turns    INT  NOT NULL DEFAULT 20,
    subgoals     JSONB NOT NULL DEFAULT '[]',
    last_verdict TEXT,
    consecutive_judge_failures INT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

- [ ] **Step 2: Register module** — add `pub mod session_goals;` to `db/mod.rs`.

- [ ] **Step 3: Write the failing tests** — create `db/session_goals.rs` with the type + pure-helper tests + a DB round-trip test (impl in Step 5):

```rust
//! Standing-goal storage (table `session_goals`).

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRow {
    pub session_id: Uuid,
    pub goal_text: String,
    pub status: String,
    pub turn_count: i32,
    pub max_turns: i32,
    pub subgoals: Vec<String>,
    pub last_verdict: Option<String>,
    pub consecutive_judge_failures: i32,
}

impl GoalRow {
    pub fn is_running(&self) -> bool {
        self.status == "active"
    }
    pub fn budget_left(&self) -> bool {
        self.turn_count < self.max_turns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str, turns: i32, max: i32) -> GoalRow {
        GoalRow { session_id: Uuid::nil(), goal_text: "g".into(), status: status.into(),
            turn_count: turns, max_turns: max, subgoals: vec![], last_verdict: None,
            consecutive_judge_failures: 0 }
    }

    #[test]
    fn is_running_and_budget() {
        assert!(row("active", 0, 20).is_running());
        assert!(!row("paused", 0, 20).is_running());
        assert!(row("active", 19, 20).budget_left());
        assert!(!row("active", 20, 20).budget_left());
    }

    async fn seed_session(pool: &PgPool) -> Uuid {
        let sid = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, agent_id, user_id, channel) VALUES ($1, 'Test', 'u', 'telegram')")
            .bind(sid).execute(pool).await.unwrap();
        sid
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn upsert_get_bump_status_roundtrip(pool: PgPool) -> sqlx::Result<()> {
        let sid = seed_session(&pool).await;
        upsert(&pool, sid, "refactor api", 5).await.unwrap();
        let g = get(&pool, sid).await.unwrap().unwrap();
        assert_eq!(g.goal_text, "refactor api");
        assert_eq!(g.max_turns, 5);
        assert!(g.is_running());
        bump_turn(&pool, sid).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().turn_count, 1);
        set_subgoals(&pool, sid, &["a".into(), "b".into()]).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().subgoals, vec!["a", "b"]);
        record_verdict(&pool, sid, "continue", true).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().consecutive_judge_failures, 1);
        record_verdict(&pool, sid, "continue", false).await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().consecutive_judge_failures, 0);
        set_status(&pool, sid, "done").await.unwrap();
        assert_eq!(get(&pool, sid).await.unwrap().unwrap().status, "done");
        clear(&pool, sid).await.unwrap();
        assert!(get(&pool, sid).await.unwrap().is_none());
        Ok(())
    }
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test --bin opex-core session_goals::tests::is_running_and_budget`
Expected: FAIL — `get`/`upsert`/etc. not found (the file won't compile).

- [ ] **Step 5: Implement the storage functions** (insert above `#[cfg(test)]`):

```rust
pub async fn get(db: &PgPool, session_id: Uuid) -> Result<Option<GoalRow>> {
    let row: Option<(String, String, i32, i32, serde_json::Value, Option<String>, i32)> = sqlx::query_as(
        "SELECT goal_text, status, turn_count, max_turns, subgoals, last_verdict, consecutive_judge_failures
         FROM session_goals WHERE session_id = $1",
    ).bind(session_id).fetch_optional(db).await?;
    Ok(row.map(|(goal_text, status, turn_count, max_turns, subgoals, last_verdict, cjf)| GoalRow {
        session_id, goal_text, status, turn_count, max_turns,
        subgoals: serde_json::from_value(subgoals).unwrap_or_default(),
        last_verdict, consecutive_judge_failures: cjf,
    }))
}

pub async fn upsert(db: &PgPool, session_id: Uuid, goal_text: &str, max_turns: i32) -> Result<()> {
    sqlx::query(
        "INSERT INTO session_goals (session_id, goal_text, status, turn_count, max_turns)
         VALUES ($1, $2, 'active', 0, $3)
         ON CONFLICT (session_id) DO UPDATE SET goal_text = EXCLUDED.goal_text,
           status = 'active', turn_count = 0, max_turns = EXCLUDED.max_turns, updated_at = now()",
    ).bind(session_id).bind(goal_text).bind(max_turns).execute(db).await?;
    Ok(())
}

pub async fn set_status(db: &PgPool, session_id: Uuid, status: &str) -> Result<()> {
    sqlx::query("UPDATE session_goals SET status = $2, updated_at = now() WHERE session_id = $1")
        .bind(session_id).bind(status).execute(db).await?;
    Ok(())
}

pub async fn bump_turn(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE session_goals SET turn_count = turn_count + 1, updated_at = now() WHERE session_id = $1")
        .bind(session_id).execute(db).await?;
    Ok(())
}

pub async fn set_subgoals(db: &PgPool, session_id: Uuid, subgoals: &[String]) -> Result<()> {
    sqlx::query("UPDATE session_goals SET subgoals = $2, updated_at = now() WHERE session_id = $1")
        .bind(session_id).bind(serde_json::to_value(subgoals)?).execute(db).await?;
    Ok(())
}

/// Record a judge verdict; reset the failure counter on a clean parse, increment on parse failure.
pub async fn record_verdict(db: &PgPool, session_id: Uuid, verdict: &str, judge_failed: bool) -> Result<()> {
    sqlx::query(
        "UPDATE session_goals SET last_verdict = $2,
           consecutive_judge_failures = CASE WHEN $3 THEN consecutive_judge_failures + 1 ELSE 0 END,
           updated_at = now() WHERE session_id = $1",
    ).bind(session_id).bind(verdict).bind(judge_failed).execute(db).await?;
    Ok(())
}

pub async fn clear(db: &PgPool, session_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM session_goals WHERE session_id = $1").bind(session_id).execute(db).await?;
    Ok(())
}
```

- [ ] **Step 6: Run pure + DB tests**

Run: `cargo test --bin opex-core session_goals::tests::is_running_and_budget` (local), then with the test postgres `DATABASE_URL=… cargo test --bin opex-core session_goals`.
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add migrations/056_session_goals.sql crates/opex-core/src/db/session_goals.rs crates/opex-core/src/db/mod.rs
git commit -m "feat(goal): session_goals table + storage (GoalRow, upsert/get/bump/verdict/clear)"
```

---

### Task 2: `agent/goal/mod.rs` — pure logic (parsers, judge, prompt, decision)

**Files:**
- Create: `crates/opex-core/src/agent/goal/mod.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (add `pub mod goal;`)

**Interfaces:**
- Consumes: `db::session_goals::GoalRow`.
- Produces: `enum Verdict { Done, Continue, ParseFail }`; `parse_judge_verdict(&str) -> Verdict`; `continuation_prompt(goal: &str, subgoals: &[String]) -> String`; `enum GoalCmd { Set(String), Status, Pause, Resume, Clear }` + `parse_goal_command(&str) -> GoalCmd`; `enum SubgoalCmd { Add(String), List, Remove(usize) }` + `parse_subgoal_command(&str) -> SubgoalCmd`; `enum DriverAction { Continue, Done, Pause(&'static str) }` + `next_action(row: &GoalRow, verdict: Verdict) -> DriverAction`.

- [ ] **Step 1: Write the failing tests** — create `agent/goal/mod.rs` with tests first:

```rust
//! `/goal` autonomous-loop pure logic: command/verdict parsing, prompt building,
//! and the driver's per-turn decision. No IO — fully unit-tested.

use crate::db::session_goals::GoalRow;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict() {
        assert_eq!(parse_judge_verdict(r#"{"done": true, "reason": "ok"}"#), Verdict::Done);
        assert_eq!(parse_judge_verdict(r#"{"done": false, "reason": "more"}"#), Verdict::Continue);
        assert_eq!(parse_judge_verdict("```json\n{\"done\": true}\n```"), Verdict::Done);
        assert_eq!(parse_judge_verdict("garbage"), Verdict::ParseFail);
        assert_eq!(parse_judge_verdict(""), Verdict::ParseFail);
    }

    #[test]
    fn parse_goal_cmd() {
        assert!(matches!(parse_goal_command("pause"), GoalCmd::Pause));
        assert!(matches!(parse_goal_command("resume"), GoalCmd::Resume));
        assert!(matches!(parse_goal_command("clear"), GoalCmd::Clear));
        assert!(matches!(parse_goal_command(""), GoalCmd::Status));
        assert!(matches!(parse_goal_command("status"), GoalCmd::Status));
        match parse_goal_command("refactor the api") { GoalCmd::Set(t) => assert_eq!(t, "refactor the api"), _ => panic!() }
    }

    #[test]
    fn parse_subgoal_cmd() {
        assert!(matches!(parse_subgoal_command("list"), SubgoalCmd::List));
        assert!(matches!(parse_subgoal_command("remove 2"), SubgoalCmd::Remove(2)));
        match parse_subgoal_command("tests pass") { SubgoalCmd::Add(t) => assert_eq!(t, "tests pass"), _ => panic!() }
    }

    #[test]
    fn continuation_includes_goal_and_subgoals() {
        let p = continuation_prompt("ship it", &["tests green".into(), "docs updated".into()]);
        assert!(p.contains("ship it"));
        assert!(p.contains("tests green") && p.contains("docs updated"));
    }

    fn row(status: &str, turns: i32, max: i32, cjf: i32) -> GoalRow {
        GoalRow { session_id: uuid::Uuid::nil(), goal_text: "g".into(), status: status.into(),
            turn_count: turns, max_turns: max, subgoals: vec![], last_verdict: None,
            consecutive_judge_failures: cjf }
    }

    #[test]
    fn decision_table() {
        assert!(matches!(next_action(&row("active", 1, 20, 0), Verdict::Done), DriverAction::Done));
        assert!(matches!(next_action(&row("active", 1, 20, 0), Verdict::Continue), DriverAction::Continue));
        // 3rd consecutive parse fail (cjf already 2 → becomes 3) → pause
        assert!(matches!(next_action(&row("active", 1, 20, 2), Verdict::ParseFail), DriverAction::Pause(_)));
        // budget exhausted + continue → pause
        assert!(matches!(next_action(&row("active", 20, 20, 0), Verdict::Continue), DriverAction::Pause(_)));
        // Done wins even on the budget-exhausting turn (Done checked before budget)
        assert!(matches!(next_action(&row("active", 20, 20, 0), Verdict::Done), DriverAction::Done));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin opex-core goal::tests`
Expected: FAIL — items not found.

- [ ] **Step 3: Implement the pure logic** (above `#[cfg(test)]`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict { Done, Continue, ParseFail }

/// Tolerant judge-output parser: strip ``` fences, take the first {...}, read `done`.
pub fn parse_judge_verdict(raw: &str) -> Verdict {
    let cleaned = raw.replace("```json", "").replace("```", "");
    let (start, end) = (cleaned.find('{'), cleaned.rfind('}'));
    let Some((s, e)) = start.zip(end) else { return Verdict::ParseFail };
    if s > e { return Verdict::ParseFail; }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned[s..=e]) else {
        return Verdict::ParseFail;
    };
    match v.get("done").and_then(|d| d.as_bool()) {
        Some(true) => Verdict::Done,
        Some(false) => Verdict::Continue,
        None => Verdict::ParseFail,
    }
}

/// The user-role message re-injected each autonomous turn.
pub fn continuation_prompt(goal: &str, subgoals: &[String]) -> String {
    let mut s = format!(
        "[autonomous continuation] Keep working toward this goal:\n{goal}\n"
    );
    if !subgoals.is_empty() {
        s.push_str("\nRanked criteria:\n");
        for (i, sg) in subgoals.iter().enumerate() {
            s.push_str(&format!("{}. {sg}\n", i + 1));
        }
    }
    s.push_str("\nWhen the goal is fully achieved, state that explicitly. Otherwise take the next concrete step.");
    s
}

pub enum GoalCmd { Set(String), Status, Pause, Resume, Clear }

pub fn parse_goal_command(arg: &str) -> GoalCmd {
    let a = arg.trim();
    match a.to_lowercase().as_str() {
        "" | "status" => GoalCmd::Status,
        "pause" => GoalCmd::Pause,
        "resume" => GoalCmd::Resume,
        "clear" => GoalCmd::Clear,
        _ => GoalCmd::Set(a.to_string()),
    }
}

pub enum SubgoalCmd { Add(String), List, Remove(usize) }

pub fn parse_subgoal_command(arg: &str) -> SubgoalCmd {
    let a = arg.trim();
    if a.eq_ignore_ascii_case("list") {
        return SubgoalCmd::List;
    }
    if let Some(rest) = a.strip_prefix("remove ").or_else(|| a.strip_prefix("remove\t")) {
        if let Ok(n) = rest.trim().parse::<usize>() {
            return SubgoalCmd::Remove(n);
        }
    }
    SubgoalCmd::Add(a.to_string())
}

pub enum DriverAction { Continue, Done, Pause(&'static str) }

/// Decide what the driver does after a turn, given the (just-reloaded) row and the judge verdict.
/// `consecutive_judge_failures` in `row` is the value BEFORE this verdict is recorded.
/// `Done` is checked BEFORE the budget so a goal completed on the budget-exhausting turn
/// is reported as done, not paused.
pub fn next_action(row: &GoalRow, verdict: Verdict) -> DriverAction {
    match verdict {
        Verdict::Done => DriverAction::Done,
        _ if !row.budget_left() => DriverAction::Pause("budget"),
        Verdict::Continue => DriverAction::Continue,
        Verdict::ParseFail => {
            if row.consecutive_judge_failures + 1 >= 3 {
                DriverAction::Pause("judge")
            } else {
                DriverAction::Continue
            }
        }
    }
}
```

- [ ] **Step 4: Add the module** — in `crates/opex-core/src/agent/mod.rs` add `pub mod goal;`.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test --bin opex-core goal::tests`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/goal/mod.rs crates/opex-core/src/agent/mod.rs
git commit -m "feat(goal): pure logic — verdict/command parsers, continuation prompt, driver decision"
```

---

### Task 3: `GoalDriverPool` + `AgentConfig` wiring

**Files:**
- Create: `crates/opex-core/src/agent/goal/pool.rs`
- Modify: `crates/opex-core/src/agent/goal/mod.rs` (`pub mod pool;`)
- Modify: `crates/opex-core/src/agent/agent_config.rs` (add fields — mirror `session_pools` at line 47)

**Interfaces:**
- Produces: `type GoalDriverPool = Arc<DashMap<Uuid, GoalDriverHandle>>`; `struct GoalDriverHandle { cancel: CancellationToken, join: JoinHandle<()>, target: Option<(String, String)> }` (target = (channel, chat_id)); methods on a thin wrapper or free fns: `is_running(pool, session_id) -> bool`, `stop(pool, session_id)`.
- Produces: `type GoalLocks = Arc<DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>`; `fn goal_lock(locks, session_id) -> Arc<Mutex<()>>`.
- Produces on `AgentConfig`: `pub goal_pool: Option<GoalDriverPool>`, `pub goal_locks: Option<GoalLocks>`.

- [ ] **Step 1: Implement `pool.rs`** (no failing-test ceremony — it is glue verified by compile + later integration). Create `crates/opex-core/src/agent/goal/pool.rs`:

```rust
use std::sync::Arc;
use dashmap::DashMap;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// (channel, chat_id) the driver delivers to; None for web sessions.
pub type GoalTarget = Option<(String, String)>;

pub struct GoalDriverHandle {
    pub cancel: CancellationToken,
    pub join: JoinHandle<()>,
    pub target: GoalTarget,
}

pub type GoalDriverPool = Arc<DashMap<Uuid, GoalDriverHandle>>;
pub type GoalLocks = Arc<DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>;

pub fn new_pool() -> GoalDriverPool { Arc::new(DashMap::new()) }
pub fn new_locks() -> GoalLocks { Arc::new(DashMap::new()) }

pub fn is_running(pool: &GoalDriverPool, session_id: Uuid) -> bool {
    pool.get(&session_id).map(|h| !h.join.is_finished()).unwrap_or(false)
}

pub fn stop(pool: &GoalDriverPool, session_id: Uuid) {
    if let Some((_, h)) = pool.remove(&session_id) {
        h.cancel.cancel();
        h.join.abort();
    }
}

/// Per-session lock that the driver and user-message entry points share.
pub fn goal_lock(locks: &GoalLocks, session_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    locks.entry(session_id).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
}
```

- [ ] **Step 2: Add to `goal/mod.rs`**: `pub mod pool;`.

- [ ] **Step 3: Add fields to `AgentConfig`** (`agent_config.rs`, after `session_pools` at line 47):

```rust
    pub goal_pool: Option<crate::agent::goal::pool::GoalDriverPool>,
    pub goal_locks: Option<crate::agent::goal::pool::GoalLocks>,
```

Then find every `AgentConfig { … }` constructor (grep `AgentConfig {`) and add `goal_pool: Some(crate::agent::goal::pool::new_pool()), goal_locks: Some(crate::agent::goal::pool::new_locks()),` (or `None` in test constructors that don't need it). `dashmap` is already a dependency (used by `session_tool_state`); confirm with `grep dashmap crates/opex-core/Cargo.toml` and add it if missing.

- [ ] **Step 4: Verify compile**

Run: `cargo check --bin opex-core`
Expected: clean (every `AgentConfig` literal updated).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/goal/pool.rs crates/opex-core/src/agent/goal/mod.rs crates/opex-core/src/agent/agent_config.rs
git commit -m "feat(goal): GoalDriverPool + per-session goal locks on AgentConfig"
```

---

### Task 4: `run_goal_turn` engine entry

**Files:**
- Modify: `crates/opex-core/src/agent/engine/run.rs` (add `run_goal_turn`, mirror `handle_isolated_via_pipeline` at lines 491–472)

**Interfaces:**
- Produces: `pub async fn run_goal_turn(&self, session_id: Uuid, prompt: &str) -> Result<String>` — runs one turn that CONTINUES `session_id` (history loaded) and returns the final assistant text.

- [ ] **Step 1: Implement** by copying `handle_isolated_via_pipeline` (run.rs:491) and changing:
  - Build the `IncomingMessage` from `prompt` (use the same `IncomingMessage` shape the cron path uses; set `channel` to the session's channel if needed, text = `prompt`).
  - `BootstrapContext { msg, resume_session_id: Some(session_id), force_new_session: false }` (instead of `None`/`true`).
  - Keep `NoopSink`, `BehaviourLayers::for_cron(&loop_config, msg)`, the tool-policy-override block, `execute`, `finalize` returning the final text.

The body is structurally identical to `handle_isolated_via_pipeline` (lines 491–472 wrap to ~565); reconcile the exact `IncomingMessage` constructor and the `boot_for_execute`/`execute`/`finalize` tail against that function.

- [ ] **Step 2: Verify compile**

Run: `cargo check --bin opex-core`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/opex-core/src/agent/engine/run.rs
git commit -m "feat(goal): run_goal_turn — continue an existing session and return final text"
```

---

### Task 5: Goal driver loop (`agent/goal/driver.rs`)

**Files:**
- Create: `crates/opex-core/src/agent/goal/driver.rs`
- Modify: `crates/opex-core/src/agent/goal/mod.rs` (`pub mod driver;`) + add `spawn_goal_driver` to `pool.rs` or `driver.rs`

**Interfaces:**
- Consumes: `run_goal_turn`, `db::session_goals::*`, `goal::{continuation_prompt, parse_judge_verdict, next_action, Verdict, DriverAction}`, `goal_lock`, the agent's `compaction_provider`/main provider, `engine.state().channel_router`.
- Produces: `pub fn spawn_goal_driver(engine: Arc<AgentEngine>, session_id: Uuid, target: GoalTarget) -> GoalDriverHandle` (mirror `spawn_live_agent`, session_agent_pool.rs:280); `async fn run_goal_driver(engine, session_id, target, cancel)`.

- [ ] **Step 1: Implement `spawn_goal_driver`** mirroring `spawn_live_agent` (session_agent_pool.rs:280): make a `CancellationToken`, `tokio::spawn(run_goal_driver(engine.clone(), session_id, target.clone(), cancel.clone()))`, return `GoalDriverHandle { cancel, join, target }`.

- [ ] **Step 2: Implement `run_goal_driver`** — the loop:

```rust
async fn run_goal_driver(engine: Arc<AgentEngine>, session_id: Uuid, target: GoalTarget, cancel: CancellationToken) {
    let db = engine.cfg().db.clone();
    let locks = match engine.cfg().goal_locks.clone() { Some(l) => l, None => return };
    loop {
        if cancel.is_cancelled() { break; }
        let Ok(Some(row)) = crate::db::session_goals::get(&db, session_id).await else { break; };
        if !row.is_running() { break; }
        if !row.budget_left() {
            let _ = crate::db::session_goals::set_status(&db, session_id, "paused").await;
            deliver(&engine, session_id, &target, &format!("⏸ Goal hit the turn budget ({}). /goal resume to continue.", row.max_turns)).await;
            break;
        }
        // Serialize against user turns.
        let lock = crate::agent::goal::pool::goal_lock(&locks, session_id);
        let text = {
            let _guard = lock.lock().await;
            if cancel.is_cancelled() { break; }
            let prompt = crate::agent::goal::continuation_prompt(&row.goal_text, &row.subgoals);
            match engine.run_goal_turn(session_id, &prompt).await {
                Ok(t) => t,
                Err(e) => { tracing::warn!(session = %session_id, error = %e, "goal turn failed"); String::new() }
            }
        };
        crate::db::session_goals::bump_turn(&db, session_id).await.ok();
        if !text.trim().is_empty() {
            deliver(&engine, session_id, &target, &text).await;
        }
        let verdict = judge(&engine, &row.goal_text, &row.subgoals, &text).await;
        let fresh = crate::db::session_goals::get(&db, session_id).await.ok().flatten().unwrap_or(row);
        let parse_failed = verdict == crate::agent::goal::Verdict::ParseFail;
        crate::db::session_goals::record_verdict(&db, session_id,
            if verdict == crate::agent::goal::Verdict::Done { "done" } else { "continue" }, parse_failed).await.ok();
        match crate::agent::goal::next_action(&fresh, verdict) {
            crate::agent::goal::DriverAction::Done => {
                let _ = crate::db::session_goals::set_status(&db, session_id, "done").await;
                deliver(&engine, session_id, &target, "✅ Goal complete.").await;
                break;
            }
            crate::agent::goal::DriverAction::Pause(reason) => {
                let _ = crate::db::session_goals::set_status(&db, session_id, "paused").await;
                let msg = if reason == "judge" { "⏸ Goal paused (judge unreliable). /goal resume to retry." }
                          else { "⏸ Goal paused (turn budget). /goal resume to continue." };
                deliver(&engine, session_id, &target, msg).await;
                break;
            }
            crate::agent::goal::DriverAction::Continue => {}
        }
    }
    if let Some(pool) = engine.cfg().goal_pool.clone() { pool.remove(&session_id); }
}
```

- [ ] **Step 3: Implement `judge`** — call the aux/compaction provider (fall back to main) with the strict judge prompt; parse with `parse_judge_verdict`; any provider error → `Verdict::Continue` (fail-open). Use `engine.cfg().compaction_provider.clone().unwrap_or_else(|| engine.provider_arc())` and the provider's `chat`/non-streaming call (mirror how `compact_if_needed`/`generate_hermes_summary` call the compaction provider in `agent/history.rs`).

- [ ] **Step 4: Implement `deliver`** — channel target → send a `send_message` `ChannelAction` via `engine.state().channel_router` (mirror `pipeline::channel_actions::send_channel_message`/`handle_message_action`); web target (None) → no-op (the turn is already persisted by `run_goal_turn`'s finalize) plus broadcast a `ui_event` via `engine.state().ui_event_tx` (`{type:"goal-turn", sessionId}`).

- [ ] **Step 5: Verify compile + the goal pure tests still pass**

Run: `cargo clippy --bin opex-core --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/goal/driver.rs crates/opex-core/src/agent/goal/mod.rs crates/opex-core/src/agent/goal/pool.rs
git commit -m "feat(goal): background goal-driver loop (run_goal_turn -> deliver -> judge -> decide)"
```

---

### Task 6: `/goal` + `/subgoal` slash commands

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/commands.rs` (add `"/goal"` + `"/subgoal"` arms)
- Modify: `crates/opex-core/src/agent/pipeline/commands.rs` `CommandContext` struct (add `agent_map` + `goal_pool` + `goal_locks` borrows) and its construction site `engine/context_builder.rs:154`

**Interfaces:**
- Consumes: `goal::{parse_goal_command, parse_subgoal_command, GoalCmd, SubgoalCmd}`, `goal::driver::spawn_goal_driver`, `goal::pool::{is_running, stop}`, `db::session_goals::*`, `cfg.agent_map`.

- [ ] **Step 1: Extend `CommandContext`** (commands.rs) with `pub agent_map: Option<&'a crate::agent::AgentMap>`, `pub goal_pool: Option<&'a crate::agent::goal::pool::GoalDriverPool>`, `pub goal_locks: Option<&'a crate::agent::goal::pool::GoalLocks>`; populate them at `engine/context_builder.rs:154` from `self.cfg().agent_map.as_ref()` / `self.cfg().goal_pool.as_ref()` / `self.cfg().goal_locks.as_ref()`.

- [ ] **Step 2: Add the `"/goal"` arm** — resolve `session_id` (the current session is available in the command context via the same path other commands use, e.g. `sessions::find_active_session` as `/usage` does; or thread it in). For `GoalCmd::Set(text)`: `upsert(ctx.db, session_id, &text, max_turns)`, resolve `Arc<AgentEngine>` via `ctx.agent_map?.get(ctx.agent_name)`, resolve `target` from the session's latest message context, `spawn_goal_driver(engine, session_id, target)` and insert into `ctx.goal_pool`. `Status`/`Pause`/`Resume`/`Clear` update status + `goal::pool::stop`/`spawn` accordingly. Replies are short status strings (English inline, as `/voice` does).

- [ ] **Step 3: Add the `"/subgoal"` arm** — `Add` appends to `get(...).subgoals` + `set_subgoals`; `List` reports; `Remove(n)` drops the 1-based index. Requires an active goal (else reply "No active goal — set one with /goal <text>.").

- [ ] **Step 4: Add a parser unit test** to the commands `mod tests`:

```rust
    #[test]
    fn goal_and_subgoal_parsers() {
        use crate::agent::goal::{parse_goal_command, GoalCmd, parse_subgoal_command, SubgoalCmd};
        assert!(matches!(parse_goal_command("pause"), GoalCmd::Pause));
        assert!(matches!(parse_subgoal_command("remove 1"), SubgoalCmd::Remove(1)));
    }
```

- [ ] **Step 5: Verify compile + tests**

Run: `cargo test --bin opex-core goal_and_subgoal_parsers` then `cargo clippy --bin opex-core --all-targets -- -D warnings`.
Expected: clean + pass.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/commands.rs crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "feat(goal): /goal + /subgoal slash commands (start/stop driver via agent_map + goal_pool)"
```

---

### Task 7: Preempt — acquire the goal lock on user turns

**Files:**
- Modify: `crates/opex-core/src/agent/engine/run.rs` (`handle_with_status`, `handle_sse_inner`)

**Interfaces:**
- Consumes: `goal::pool::{is_running, goal_lock}`, `cfg.goal_pool`, `cfg.goal_locks`.

- [ ] **Step 1: Guard both user entry points.** At the top of `handle_with_status` and `handle_sse_inner`, after `session_id` is known and only when a goal driver is running for it, acquire the per-session goal lock for the duration of the turn:

```rust
        // Serialize user turns against an active goal driver (no-op when no goal).
        let _goal_guard = match (self.cfg().goal_pool.as_ref(), self.cfg().goal_locks.as_ref()) {
            (Some(p), Some(l)) if crate::agent::goal::pool::is_running(p, session_id) => {
                Some(crate::agent::goal::pool::goal_lock(l, session_id).lock_owned().await)
            }
            _ => None,
        };
```

Place it after `session_id` resolution and before `execute` so the guard lives across the turn. (Both methods resolve `session_id` from the bootstrap outcome.)

- [ ] **Step 2: Verify compile**

Run: `cargo clippy --bin opex-core --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/opex-core/src/agent/engine/run.rs
git commit -m "feat(goal): serialize user turns against the goal driver via per-session lock"
```

---

### Task 8: Web UI — append autonomous turns from the `goal-turn` event

**Files:**
- Modify: the chat store / SSE-event handling (`ui/src/stores/sse-events.ts` + `ui/src/stores/chat-store.ts` or the notifications WS handler) to react to a `goal-turn` ui_event by refetching/appending the session's latest message.

**Interfaces:**
- Consumes: the `ui_event` broadcast (`{type:"goal-turn", sessionId}`) emitted by the driver's `deliver` (Task 5 Step 4).

- [ ] **Step 1: Handle the event** — in the existing ui_event/WS handler, on `type === "goal-turn"` for the currently-open session, trigger the same "reload session messages" path the UI already uses after a turn. (If no such WS handler subscribes to ui_event in the chat view, the minimum viable behaviour is: the turns are persisted, so a manual refresh shows them — note this limitation and keep the event wiring for when the chat view subscribes.)

- [ ] **Step 2: Verify**

Run: `cd ui && npm test` and `cd ui && npm run build`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add ui/
git commit -m "feat(ui/chat): append autonomous goal turns on goal-turn event"
```

---

## Final verification & deploy

- [ ] `cargo clippy --bin opex-core --all-targets -- -D warnings` — clean.
- [ ] Test postgres up; `DATABASE_URL=… cargo test --bin opex-core` — full suite incl. `session_goals` + `goal` green.
- [ ] `cd ui && npm test && npm run build`.
- [ ] Deploy: `make remote-deploy` (syncs migration 056) + UI deploy + `make doctor`.
- [ ] Server smoke (Telegram): `/goal <small task>` → agent runs several turns, each delivered to chat, stops at done or budget; `/goal status|pause|resume|clear`; `/subgoal add/list/remove`; send a normal message mid-loop and confirm it is handled and the loop continues after.

## Self-review checklist (completed by plan author)

- **Spec coverage:** storage→T1; pure logic→T2; pool+config→T3; run_goal_turn→T4; driver/judge/deliver→T5; commands→T6; preempt lock→T7; web event→T8. All 7 spec components mapped.
- **Placeholder scan:** full code for the testable units (T1, T2) and the driver loop (T5 Step 2); integration steps (T4, T5 Steps 3–4, T6, T8) reference exact precedents (file:line) to mirror — flagged up front under Global Constraints, not vague placeholders.
- **Type consistency:** `GoalRow`, `Verdict`, `DriverAction`, `GoalCmd`/`SubgoalCmd`, `next_action(row, verdict)`, `continuation_prompt(goal, subgoals)`, `parse_judge_verdict`, `spawn_goal_driver(engine, session_id, target)`, `goal_lock`, `GoalDriverPool`/`GoalLocks` consistent across tasks.
