# Auto-Approve Day Plan with Daily Token Budget — Design

**Date:** 2026-07-14
**Status:** Approved design, pre-implementation
**Area:** `crates/opex-core/src/agent/initiative/day_plan.rs`, `crates/opex-core/src/config/mod.rs`, `crates/opex-core/src/agent/initiative/delivery.rs`

## 1. Problem

B-wide's morning day plan (`agent_plans.day_plan` over `session_goals`, shipped
2026-07-13) always waits for the owner to approve it with a one-tap
(`POST /api/agents/{name}/plan/day/{date}/approve`, Telegram `dpm:approve:{date}`).
The owner wants an **opt-in auto-approve** so they don't tap every morning —
while keeping **cost control** so an autonomous plan can't run up unbounded
spend.

## 2. Goal

Add an opt-in per-agent mode where the morning day plan is materialized
automatically (no tap), bounded by a **daily token ceiling**: the plan's
advancement pauses for the day once the agent's token usage today reaches the
cap. Default off — no behaviour change for existing agents.

Decisions (locked in brainstorming):

- **Mechanism:** runtime daily spend ceiling — auto-approve always (when opted
  in and under budget); advancement pauses when the cap is hit.
- **Metric:** tokens/day, via the existing
  `opex_db::usage::get_agent_usage_today(db, agent_id)` (SUM of
  `input_tokens + output_tokens` since `CURRENT_DATE`). No new SQL.
- **Scope of spend:** the agent's **entire** daily usage (owner chat +
  autonomous + heartbeat) — one number, `get_agent_usage_today` as-is.
- **Budget applies to auto-approved plans only.** A manually approved plan is
  explicit owner consent for the whole plan → today's unbounded advancement is
  unchanged.

## 3. Design (Approach A)

Self-contained in `day_plan.rs` + config + two notification helpers. Reuses the
existing CAS-guarded, idempotent `materialize_day_plan_tx` (from
`gateway/handlers/agents/initiative.rs`) — the exact function the manual
approve endpoint calls.

### 3.1 Config (opt-in) — `config/mod.rs`

`InitiativeConfig` gains two fields:

```rust
#[serde(default)]
pub auto_approve_day_plan: bool,   // default false
#[serde(default)]
pub daily_token_budget: u64,       // default 0 (= unset)
```

`InitiativeConfig::validate()` adds: if `auto_approve_day_plan` is true, then
`daily_plan` MUST be true AND `daily_token_budget` MUST be > 0 — otherwise a
config error (mirrors the existing `daily_plan ⇒ decompose` style validation).
Preserves the opt-in chain: `enabled → daily_plan → auto_approve_day_plan`.

### 3.2 Pure budget decision — `day_plan.rs`

A single pure helper, unit-testable without a DB:

```rust
/// Is the agent still under its daily token ceiling? `budget == 0` means the
/// cap is unset — treated as "not under budget" for the auto path (but the
/// auto path is only reached when validate() guaranteed budget > 0, so this is
/// a defensive floor).
pub(crate) fn within_token_budget(spend_today: i64, budget: u64) -> bool {
    budget > 0 && (spend_today as u64) < budget
}
```

Both call sites read `get_agent_usage_today` once and pass the value in.

### 3.3 Auto-approve at generation — `day_plan_tick_inner`

In the generation branch (new day), after the plan is written with
`set_day_plan(..., Some("pending"))` and the owner is notified via the existing
`notify_day_plan`, add:

```
if deps.cfg.auto_approve_day_plan {
    let spend = get_agent_usage_today(db, agent).await.unwrap_or(0);
    if within_token_budget(spend, deps.cfg.daily_token_budget) {
        let n = materialize_day_plan_tx(db, agent, today).await?; // CAS-guarded
        if n > 0 { notify_day_plan_auto_approved(db, engine, agent, deps, &intents, today).await; }
    }
    // else: over budget at generation → leave pending; the buttons from
    // notify_day_plan remain the manual fallback (no extra work).
}
```

- `materialize_day_plan_tx` is idempotent/CAS-guarded on `(pending, date)`, so a
  race with an owner tap is safe (one wins, the other is a no-op).
- The auto-approved notification enumerates all intents (informed consent
  preserved) but carries **no** approve/dismiss buttons — it is informational.

### 3.4 Runtime pause at advancement — `advance_day_plan`

At the top of `advance_day_plan` (reached only when `day_plan_status ==
"approved"`), before doing any work:

```
if deps.cfg.auto_approve_day_plan {
    let spend = get_agent_usage_today(db, agent).await.unwrap_or(0);
    if !within_token_budget(spend, deps.cfg.daily_token_budget) {
        let _ = agent_plans::set_day_plan_status(db, agent, Some("paused")).await;
        notify_day_plan_paused(db, engine, agent, deps, deps.cfg.daily_token_budget).await;
        return;
    }
}
```

- New `day_plan_status` value `"paused"`. It is terminal for the rest of the day:
  `day_plan_tick_inner` only advances when status is `"approved"`, so a paused
  plan does no more work today.
- A fresh day re-enters the generation branch (`plan.day_plan_date != today`)
  regardless of the paused status, so the next morning regenerates normally. The
  prev-day finalize loop (already present) sets any lingering `active` intents'
  session_goals to `paused` — unchanged, still correct.
- The guard is gated on `auto_approve_day_plan`, so **manually approved plans
  never pause** (§2 decision).

### 3.5 Notifications — `delivery.rs`

Two helpers, localized (Russian, matching existing `send_day_plan_to_channel` /
`notify_plan_done` copy), routed via `resolve_owner_target` + the channel
router, plus a UI notification for the auto-approved case:

- `notify_day_plan_auto_approved(...)` — "🤖 {agent}: план на день принят
  автоматически (N намерений)" + the enumerated intents (reuse
  `day_plan_body`). UI `notify(..., "day_plan", ...)` too.
- `notify_day_plan_paused(...)` — "⏸ {agent}: дневной лимит {cap} токенов
  достигнут — план приостановлен до завтра."

These mirror the existing `notify_day_plan` / `notify_plan_done` structure
(oneshot reply + 5s timeout).

## 4. Data flow

```text
morning tick (day changed):
  generate intents → set_day_plan(pending) → notify_day_plan (buttons)
    if auto_approve && spend_today < budget:
        materialize_day_plan_tx (pending→approved, N session_goals) → notify auto-approved
    else if auto_approve && over budget:
        stay pending (manual buttons are the fallback)

later ticks (status == approved):
  advance_day_plan:
    if auto_approve && spend_today >= budget:
        status → paused, notify paused, stop
    else:
        advance_one_chunk (unchanged)
```

## 5. Error handling

- `get_agent_usage_today` failure → `unwrap_or(0)` (treat as "no spend": fail
  toward auto-approving / continuing — a DB read failure must not silently
  strand the plan; the next tick re-checks). Documented at both call sites.
- `materialize_day_plan_tx` returning `Ok(0)` (CAS no-op, e.g. owner tapped
  first) → no auto-approved notification; not an error.
- All of `day_plan_tick` remains fail-soft (the existing outer wrapper logs and
  swallows).

## 6. Testing

- **Pure:** `within_token_budget` — under/at/over cap, `budget == 0` → false,
  saturating cast for negative `spend_today` (defensive) → treated as under.
- **sqlx:** seed a pending day plan; with `auto_approve` config + spend under
  cap → a heartbeat-equivalent call materializes it (status `approved`, N
  session_goals); with spend over cap at advancement → status flips to
  `"paused"` and no chunk advances. (Model spend by inserting `usage_log` rows
  for the agent dated today via `record_usage`/direct insert.)
- **Config:** `validate()` rejects `auto_approve_day_plan=true` with
  `daily_plan=false` or `daily_token_budget=0`; accepts a well-formed config.

## 7. Non-goals

- No generic hot-path (execute.rs) spend cap for all autonomous work (Approach
  B) — out of scope; this feature is day-plan-only.
- No same-day resume / un-pause button — paused is until tomorrow (a future
  add). Owner can still start a manual chat with the agent (that path is not
  gated).
- No USD/cost-based budget — tokens only (existing query, provider-agnostic).
- Budget does not apply to manually approved plans.
- No change to `MAX_DAY_INTENTS` (4) or per-goal `max_turns` (20) — those
  remain the per-plan structural bounds.

## 8. Rollout

Opt-in, default off. Enable on the Arty canary by adding to its
`[agent.initiative]`:

```toml
auto_approve_day_plan = true
daily_token_budget = 200000   # example
```

(requires `daily_plan = true`, already set). No migration — the two new
`day_plan_status` reader/writer paths already treat the column as free-form
text; `"paused"` is a new value, not a schema change.
