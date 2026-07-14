# Auto-Approve Day Plan with Daily Token Budget — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Opt-in per-agent auto-approve of the morning day plan (no owner tap), bounded by a daily token ceiling that pauses the plan's advancement once the agent's usage-today reaches the cap.

**Architecture:** Reuse the existing CAS-guarded `materialize_day_plan_tx` and `get_agent_usage_today`. Add two config fields, a pure `within_token_budget` decision, an auto-approve branch at plan generation, and a pause guard at advancement — all self-contained in `day_plan.rs` + `config/mod.rs` + notification helpers in `delivery.rs`.

**Tech Stack:** Rust 2024, crate `opex-core`; PostgreSQL via sqlx. No new dependencies, no migration.

**Spec:** `docs/superpowers/specs/2026-07-14-auto-approve-day-plan-budget-design.md`

## Global Constraints

- **Opt-in, default off.** No behaviour change for any agent until `[agent.initiative] auto_approve_day_plan = true` is set.
- **Budget applies to auto-approved plans ONLY.** A manually approved plan keeps today's unbounded advancement (explicit owner consent).
- **Metric:** `crate::db::usage::get_agent_usage_today(db, agent) -> Result<i64>` (SUM input+output tokens since `CURRENT_DATE`, whole-agent). On error → `unwrap_or(0)` (fail toward auto/continue; next tick re-checks).
- **`"paused"` is a new free-form value of `agent_plans.day_plan_status`** — no schema change, no migration. Written via existing `set_day_plan_status(db, agent, Some("paused"))`.
- **Non-base only** — `day_plan_tick_inner` already returns early when `deps.is_base`, so the auto path never runs for base agents; do not remove that guard.
- **Reuse, don't re-implement:** auto-approve calls `crate::gateway::handlers::agents::initiative::materialize_day_plan_tx(db, agent, date)` (CAS-guarded on `(pending, date)`, idempotent) — a race with an owner tap is safe.
- **`day_plan_tick` stays fail-soft** (outer wrapper logs+swallows); the auto/pause branches must not introduce a `?` that aborts the tick on a transient DB read.
- **Validation:** `auto_approve_day_plan = true` requires `daily_plan = true` AND `daily_token_budget > 0`.
- **Platform:** `cargo check` + `cargo clippy -p opex-core --all-targets -- -D warnings` locally (Windows); `opex-core` unit tests + `#[sqlx::test]` run on the server (need `DATABASE_URL` / `make test-db`). Each test step gives the command; note PASS as pending server run if the local runner is unavailable.
- master, one commit per task, NO `Co-Authored-By`.

---

### Task 1: Config — opt-in fields + validation

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` — `InitiativeConfig` struct (~1485-1494), its `Default` impl (~1500-1509), and `validate()` (~1511-1519). Add a `#[cfg(test)] mod` test if none covers `InitiativeConfig::validate` (add near the struct).

**Interfaces:**
- Produces: `InitiativeConfig { auto_approve_day_plan: bool, daily_token_budget: u64, .. }` — read by Task 3 as `deps.cfg.auto_approve_day_plan` / `deps.cfg.daily_token_budget`.

- [ ] **Step 1: Write the failing validation test**

Add to `crates/opex-core/src/config/mod.rs` inside a `#[cfg(test)] mod initiative_config_tests { use super::*;` block (create it just after the `impl InitiativeConfig` block):

```rust
#[cfg(test)]
mod initiative_config_tests {
    use super::*;

    fn base() -> InitiativeConfig {
        InitiativeConfig { enabled: true, daily_proposal_cap: 1, decompose: true, daily_plan: true, auto_approve_day_plan: false, daily_token_budget: 0 }
    }

    #[test]
    fn auto_approve_requires_daily_plan_and_budget() {
        // valid: auto on, daily_plan on, budget > 0
        let ok = InitiativeConfig { auto_approve_day_plan: true, daily_token_budget: 100_000, ..base() };
        assert!(ok.validate().is_empty(), "well-formed auto-approve config must pass: {:?}", ok.validate());

        // invalid: auto on but daily_plan off
        let no_plan = InitiativeConfig { auto_approve_day_plan: true, daily_token_budget: 100_000, daily_plan: false, ..base() };
        assert!(no_plan.validate().iter().any(|e| e.contains("daily_plan")), "must require daily_plan");

        // invalid: auto on but budget == 0
        let no_budget = InitiativeConfig { auto_approve_day_plan: true, daily_token_budget: 0, ..base() };
        assert!(no_budget.validate().iter().any(|e| e.contains("daily_token_budget")), "must require budget > 0");

        // auto off → no auto-related errors regardless of budget/plan
        let off = InitiativeConfig { auto_approve_day_plan: false, daily_token_budget: 0, daily_plan: false, ..base() };
        assert!(off.validate().is_empty(), "auto off must not add errors");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core initiative_config_tests`
Expected: FAIL — the fields `auto_approve_day_plan` / `daily_token_budget` do not exist yet (compile error).

- [ ] **Step 3: Add the two fields to the struct**

In `crates/opex-core/src/config/mod.rs`, in `pub struct InitiativeConfig` (after `pub daily_plan: bool,`):

```rust
    #[serde(default)]
    pub daily_plan: bool,
    /// Opt-in: materialize the morning day plan automatically (no owner tap),
    /// bounded by `daily_token_budget`. Requires `daily_plan = true`.
    #[serde(default)]
    pub auto_approve_day_plan: bool,
    /// Daily token ceiling (input+output, whole-agent) for the auto-approved
    /// day plan. Advancement pauses for the day once usage-today reaches it.
    /// Must be > 0 when `auto_approve_day_plan` is true.
    #[serde(default)]
    pub daily_token_budget: u64,
```

- [ ] **Step 4: Add the fields to the `Default` impl**

In the `impl Default for InitiativeConfig`, add to the returned struct (after `daily_plan: false,`):

```rust
            daily_plan: false,
            auto_approve_day_plan: false,
            daily_token_budget: 0,
```

- [ ] **Step 5: Add validation**

In `InitiativeConfig::validate()`, before `errors`:

```rust
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !(1..=10).contains(&self.daily_proposal_cap) {
            errors.push("initiative.daily_proposal_cap must be in [1, 10]".to_string());
        }
        if self.auto_approve_day_plan {
            if !self.daily_plan {
                errors.push("initiative.auto_approve_day_plan requires daily_plan = true".to_string());
            }
            if self.daily_token_budget == 0 {
                errors.push("initiative.daily_token_budget must be > 0 when auto_approve_day_plan is true".to_string());
            }
        }
        errors
    }
```

- [ ] **Step 6: Run check, clippy, and the test**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings` then `cargo test -p opex-core --bin opex-core initiative_config_tests`
Expected: check + clippy clean; test PASSES.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(initiative): config for auto-approve day plan + daily token budget"
```

---

### Task 2: Notification helpers (auto-approved / paused)

**Files:**
- Modify: `crates/opex-core/src/agent/initiative/delivery.rs` — add two pure text builders + a `#[cfg(test)]` test.
- Modify: `crates/opex-core/src/agent/initiative/day_plan.rs` — add two `async` notify wrappers (mirroring the existing `notify_plan_done`).

**Interfaces:**
- Consumes: existing `day_plan_body(intents: &[String]) -> String`, `resolve_owner_target`, `ChannelAction`/router, `notify(...)`.
- Produces (used by Task 3):
  - `async fn notify_day_plan_auto_approved(db, engine, agent, deps: &InitiativeDeps, intents: &[DayIntent], date: chrono::NaiveDate)`
  - `async fn notify_day_plan_paused(db, engine, agent, deps: &InitiativeDeps, cap: u64)`
  - pure: `delivery::day_plan_paused_text(agent: &str, cap: u64) -> String`, `delivery::day_plan_auto_approved_body(agent: &str, intents: &[String]) -> String`

- [ ] **Step 1: Write the failing pure-text tests**

Add to `crates/opex-core/src/agent/initiative/delivery.rs` inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn paused_text_names_agent_and_cap() {
        let s = super::day_plan_paused_text("Arty", 200_000);
        assert!(s.contains("Arty"));
        assert!(s.contains("200000"));
    }

    #[test]
    fn auto_approved_body_has_header_and_all_intents() {
        let s = super::day_plan_auto_approved_body("Arty", &["довести X".to_string(), "разобрать Y".to_string()]);
        assert!(s.contains("Arty"));
        assert!(s.contains("автоматически"));
        assert!(s.contains("довести X") && s.contains("разобрать Y"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p opex-core --bin opex-core initiative::delivery::tests`
Expected: FAIL — `day_plan_paused_text` / `day_plan_auto_approved_body` not defined.

- [ ] **Step 3: Add the pure text builders to `delivery.rs`**

Add after `day_plan_body` (line ~45):

```rust
/// Pure: informational auto-approve message — header + numbered intents.
pub(crate) fn day_plan_auto_approved_body(agent: &str, intents: &[String]) -> String {
    format!("🤖 {agent}: план на день принят автоматически\n{}", day_plan_body(intents))
}

/// Pure: pause notice when the daily token budget is reached.
pub(crate) fn day_plan_paused_text(agent: &str, cap: u64) -> String {
    format!("⏸ {agent}: дневной лимит {cap} токенов достигнут — план приостановлен до завтра")
}
```

- [ ] **Step 4: Run the pure-text tests to verify they pass**

Run: `cargo test -p opex-core --bin opex-core initiative::delivery::tests`
Expected: PASS.

- [ ] **Step 5: Add the two notify wrappers to `day_plan.rs`**

In `crates/opex-core/src/agent/initiative/day_plan.rs`, after `notify_plan_done` (line ~205), add. These mirror `notify_plan_done` (inline `send_message` via router + 5s bounded wait) and, for auto-approved, also a UI notification like `notify_day_plan`:

```rust
/// Inform the owner the day plan was auto-approved (no buttons — informational;
/// all intents enumerated for informed consent). UI notification + channel message.
async fn notify_day_plan_auto_approved(db: &PgPool, engine: &AgentEngine, agent: &str, deps: &InitiativeDeps, intents: &[DayIntent], date: chrono::NaiveDate) {
    let texts: Vec<String> = intents.iter().map(|i| i.intent.clone()).collect();
    if let Some(tx) = &deps.ui_event_tx {
        let _ = crate::gateway::handlers::notifications::notify(
            db, tx, "day_plan", &format!("{agent}: план на день (авто)"),
            &crate::agent::initiative::delivery::day_plan_body(&texts),
            serde_json::json!({ "agent": agent, "intents": texts, "date": date.to_string(), "auto_approved": true }),
        ).await;
    }
    let _ = engine;
    if let (Some(router), Some((ch, chat_id))) = (
        deps.channel_router.as_ref(),
        crate::agent::initiative::delivery::resolve_owner_target(db, agent, deps.owner_id.as_deref()).await,
    ) {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let action = crate::agent::channel_actions::ChannelAction {
            name: "send_message".to_string(),
            params: serde_json::json!({ "text": crate::agent::initiative::delivery::day_plan_auto_approved_body(agent, &texts) }),
            context: serde_json::json!({ "chat_id": chat_id }),
            reply: reply_tx, target_channel: Some(ch),
        };
        if router.send(action).await.is_ok() { let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await; }
    }
}

/// Inform the owner that the auto-approved plan paused on hitting the token budget.
async fn notify_day_plan_paused(db: &PgPool, engine: &AgentEngine, agent: &str, deps: &InitiativeDeps, cap: u64) {
    let _ = engine;
    if let (Some(router), Some((ch, chat_id))) = (
        deps.channel_router.as_ref(),
        crate::agent::initiative::delivery::resolve_owner_target(db, agent, deps.owner_id.as_deref()).await,
    ) {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let action = crate::agent::channel_actions::ChannelAction {
            name: "send_message".to_string(),
            params: serde_json::json!({ "text": crate::agent::initiative::delivery::day_plan_paused_text(agent, cap) }),
            context: serde_json::json!({ "chat_id": chat_id }),
            reply: reply_tx, target_channel: Some(ch),
        };
        if router.send(action).await.is_ok() { let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await; }
    }
}
```

- [ ] **Step 6: Run check + clippy**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: clean. (The two new `async fn` are not yet called — Rust `dead_code` will warn under `-D warnings`. To avoid a spurious failure between tasks, add `#[allow(dead_code)]` on both wrappers with a comment `// wired in Task 3`; remove the allow in Task 3 Step 6.)

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/initiative/delivery.rs crates/opex-core/src/agent/initiative/day_plan.rs
git commit -m "feat(initiative): auto-approved + budget-paused day-plan notifications"
```

---

### Task 3: Budget decision + wire auto-approve & pause into the tick

**Files:**
- Modify: `crates/opex-core/src/agent/initiative/day_plan.rs` — add pure `within_token_budget`; auto-approve branch in `day_plan_tick_inner` generation path (~116-118); pause guard at the top of `advance_day_plan` (~127-135); unit + sqlx tests in the `#[cfg(test)] mod tests`.

**Interfaces:**
- Consumes: Task 1 `deps.cfg.auto_approve_day_plan` / `deps.cfg.daily_token_budget`; Task 2 `notify_day_plan_auto_approved` / `notify_day_plan_paused`; `crate::db::usage::get_agent_usage_today`; `crate::gateway::handlers::agents::initiative::materialize_day_plan_tx`; `agent_plans::set_day_plan_status`.

- [ ] **Step 1: Write the failing unit test for `within_token_budget`**

Add to the `#[cfg(test)] mod tests` in `crates/opex-core/src/agent/initiative/day_plan.rs`:

```rust
    #[test]
    fn within_token_budget_gate() {
        assert!(super::within_token_budget(0, 100));       // fresh day, under
        assert!(super::within_token_budget(99, 100));      // just under
        assert!(!super::within_token_budget(100, 100));    // at cap → not under
        assert!(!super::within_token_budget(150, 100));    // over
        assert!(!super::within_token_budget(0, 0));        // unset budget → never "under"
        assert!(!super::within_token_budget(-5, 100));     // defensive: negative spend treated as over (saturating)
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core day_plan::tests::within_token_budget_gate`
Expected: FAIL — `within_token_budget` not defined.

- [ ] **Step 3: Add the pure `within_token_budget`**

In `crates/opex-core/src/agent/initiative/day_plan.rs`, after `MAX_DAY_INTENTS` (line ~17):

```rust
/// Pure: is the agent still under its daily token ceiling? `budget == 0` means
/// unset → never under (the auto path is only reached when validate() ensured
/// budget > 0; this is a defensive floor). Negative `spend_today` (impossible —
/// SUM ≥ 0) saturates to a huge u64 → treated as over budget.
pub(crate) fn within_token_budget(spend_today: i64, budget: u64) -> bool {
    let spent = u64::try_from(spend_today).unwrap_or(u64::MAX);
    budget > 0 && spent < budget
}
```

- [ ] **Step 4: Run the unit test to verify it passes**

Run: `cargo test -p opex-core --bin opex-core day_plan::tests::within_token_budget_gate`
Expected: PASS.

- [ ] **Step 5: Wire the auto-approve branch into `day_plan_tick_inner`**

In `day_plan_tick_inner`, the generation branch currently ends (line ~116-118):

```rust
        agent_plans::set_day_plan(db, agent, &intents, today, Some("pending")).await?;
        notify_day_plan(db, engine, agent, deps, &intents, today).await; // Task 6 provides (date → button)
        return Ok(());
```

Insert the auto-approve block between `notify_day_plan` and `return`:

```rust
        agent_plans::set_day_plan(db, agent, &intents, today, Some("pending")).await?;
        notify_day_plan(db, engine, agent, deps, &intents, today).await;
        if deps.cfg.auto_approve_day_plan {
            let spend = crate::db::usage::get_agent_usage_today(db, agent).await.unwrap_or(0);
            if within_token_budget(spend, deps.cfg.daily_token_budget) {
                // CAS-guarded/idempotent — a race with an owner tap is safe.
                match crate::gateway::handlers::agents::initiative::materialize_day_plan_tx(db, agent, today).await {
                    Ok(n) if n > 0 => {
                        // Re-read materialized intents (now with session_ids/active) for the notice.
                        let plan2 = agent_plans::get_or_create(db, agent).await?;
                        let materialized: Vec<DayIntent> = serde_json::from_value(plan2.day_plan.clone()).unwrap_or_default();
                        notify_day_plan_auto_approved(db, engine, agent, deps, &materialized, today).await;
                    }
                    Ok(_) => {} // CAS no-op (owner tapped first / empty) — not an error
                    Err(e) => tracing::warn!(agent, error = ?e, "auto-approve materialize failed (fail-soft)"),
                }
            }
            // else: over budget at generation → stay pending; notify_day_plan's
            // buttons are the manual fallback (no extra work).
        }
        return Ok(());
```

- [ ] **Step 6: Wire the pause guard into `advance_day_plan`**

In `advance_day_plan`, immediately after the function opens (before `let mut intents = ...`, line ~128), add the budget guard, then remove the `#[allow(dead_code)]` added on the two notify wrappers in Task 2:

```rust
async fn advance_day_plan(db: &PgPool, engine: &AgentEngine, agent: &str, deps: &InitiativeDeps, plan: agent_plans::PlanRow) {
    // Budget pause applies to AUTO-approved plans only (manual approve = explicit
    // consent, unbounded). Fail-soft: a usage-read error reads as 0 → continue.
    if deps.cfg.auto_approve_day_plan {
        let spend = crate::db::usage::get_agent_usage_today(db, agent).await.unwrap_or(0);
        if !within_token_budget(spend, deps.cfg.daily_token_budget) {
            let _ = agent_plans::set_day_plan_status(db, agent, Some("paused")).await;
            notify_day_plan_paused(db, engine, agent, deps, deps.cfg.daily_token_budget).await;
            return;
        }
    }
    let mut intents: Vec<DayIntent> = serde_json::from_value(plan.day_plan.clone()).unwrap_or_default();
    // ... rest unchanged ...
```

- [ ] **Step 7: Write the sqlx seam test (usage-today metric)**

Add to the `#[cfg(test)] mod tests` in `day_plan.rs` (mirrors the B-wide sqlx test style — `#[sqlx::test(migrations = "../../migrations")]`):

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn usage_today_reflects_seeded_row(pool: sqlx::PgPool) -> sqlx::Result<()> {
        // Seed a usage_log row dated today for agent "BQ" → get_agent_usage_today
        // returns the summed tokens the pause guard reads.
        sqlx::query(
            "INSERT INTO usage_log (agent_id, provider, model, input_tokens, output_tokens, status) \
             VALUES ($1, 'p', 'm', 120, 80, 'ok')",
        ).bind("BQ").execute(&pool).await.unwrap();
        let used = crate::db::usage::get_agent_usage_today(&pool, "BQ").await.unwrap();
        assert_eq!(used, 200);
        assert!(!super::within_token_budget(used, 150), "200 over a 150 cap → pause");
        assert!(super::within_token_budget(used, 500), "200 under a 500 cap → continue");
        Ok(())
    }
```

(Confirm the `usage_log` insert columns against `crates/opex-db/src/usage.rs::record_usage` / `insert_aborted_row` — `agent_id, provider, model, input_tokens, output_tokens, status` are the non-null columns those inserts use; add `session_id` only if the column is NOT NULL, which it is not.)

- [ ] **Step 8: Run check, clippy, and the tests**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings` then (server / DATABASE_URL) `cargo test -p opex-core --bin opex-core day_plan::tests`
Expected: check + clippy clean (dead_code allow removed, all wrappers now called); `within_token_budget_gate` passes locally; `usage_today_reflects_seeded_row` passes on the server (needs Postgres).

- [ ] **Step 9: Commit**

```bash
git add crates/opex-core/src/agent/initiative/day_plan.rs
git commit -m "feat(initiative): auto-approve day plan under daily token budget + pause"
```

---

## Self-Review

**Spec coverage:**
- §3.1 config fields + validation → Task 1.
- §3.2 pure `within_token_budget` → Task 3 Steps 1-4.
- §3.3 auto-approve at generation (materialize + notify, over-budget fallback) → Task 3 Step 5.
- §3.4 runtime pause (`"paused"` status, auto-only gate) → Task 3 Step 6.
- §3.5 notifications (auto-approved + paused, localized) → Task 2.
- §6 testing (pure gate, config validation, usage seam) → Task 1 Step 1, Task 2 Step 1, Task 3 Steps 1/7.
- §7 non-goals honoured: no hot-path cap, no resume button, tokens-only, manual plans unbudgeted, MAX_DAY_INTENTS/max_turns untouched.

**Placeholder scan:** none — every code step carries complete code and an exact command. The `usage_log` column note in Task 3 Step 7 asks the implementer to confirm against the real insert, not to invent.

**Type consistency:** `within_token_budget(i64, u64) -> bool`, `daily_token_budget: u64`, `auto_approve_day_plan: bool`, `notify_day_plan_auto_approved(.., &[DayIntent], NaiveDate)`, `notify_day_plan_paused(.., u64)`, `day_plan_auto_approved_body(&str, &[String])`, `day_plan_paused_text(&str, u64)` — used identically across Tasks 2 and 3. `materialize_day_plan_tx` / `get_agent_usage_today` / `set_day_plan_status` signatures match the current code (verified during planning).
