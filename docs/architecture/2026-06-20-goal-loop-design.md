# `/goal` autonomous loop — design (Phase 2b)

- **Date:** 2026-06-20
- **Status:** Approved (design); pending implementation plan
- **Branch:** `feat/goal-loop`
- **Origin:** Hermes gap analysis (`reference_hermes_agent.md`). Hermes ref: `hermes_cli/goals.py`, `gateway/slash_commands.py::_handle_goal_command`.

## Context & motivation

A channel user sets a standing goal with `/goal <text>`. After each agent turn, an
auxiliary "judge" model decides whether the goal is satisfied. If not, the agent keeps
working autonomously — turn after turn — until the goal is done, a turn budget is
exhausted, the user pauses/clears it, or a real user message preempts it. This is the
single most-requested productivity feature for gateway users (it removes the need to
keep nudging the agent).

**Scope: full** — `/goal` + `/goal status|pause|resume|clear`, `/subgoal` ranked
criteria, configurable judge model, and best-effort web-UI support.

**Continuation driver: A (background goal-driver task)** — a per-session tokio task,
decoupled from the channel WebSocket connection, that drives turns and delivers output
via the `channel_router` (channels) or persistence + a UI event (web). Chosen over
re-queuing into the channel-WS dispatcher (B, higher risk to sensitive concurrency code)
and a self-rescheduling cron (C, wrong session model).

## Cross-cutting principles

- TDD; rustls-only; clippy `-D warnings` clean; no `Co-Authored-By`; no push unless asked.
- Migrations runtime-loaded; `make remote-deploy` syncs them (fixed 2026-06-20).
- Rust application-tree tests run under `cargo test --bin hydeclaw-core`; DB tests use the
  test postgres + `#[sqlx::test(migrations = "../../migrations")]`.
- Pure logic (command parsing, judge JSON parsing, GoalState transitions) verified locally;
  the full autonomous loop + delivery verified on the server.

## Out of scope (deferred)
- Auto-resuming active goals after a process restart (drivers are in-memory; user runs
  `/goal resume`). A row stays `active` in the DB but no driver runs until resumed.
- A hard token-cost budget (turn-count budget only for v1).
- Live SSE streaming of autonomous turns to web (web gets persisted turns + a UI event
  to append; no per-turn streaming connection).

---

## Component 1 — Goal state storage

**Migration `migrations/056_session_goals.sql`:**
```sql
CREATE TABLE session_goals (
    session_id   UUID PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    goal_text    TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'active'
                 CHECK (status IN ('active', 'paused', 'done', 'cleared')),
    turn_count   INT  NOT NULL DEFAULT 0,
    max_turns    INT  NOT NULL DEFAULT 20,
    subgoals     JSONB NOT NULL DEFAULT '[]',          -- ranked array of strings
    last_verdict TEXT,                                 -- 'done' | 'continue' | NULL
    consecutive_judge_failures INT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

**`crates/hydeclaw-core/src/db/session_goals.rs` (new):** a `GoalRow` struct mirroring
the columns, plus `get(db, session_id) -> Option<GoalRow>`, `upsert(db, session_id,
goal_text, max_turns)`, `set_status`, `bump_turn`, `set_subgoals`, `record_verdict(db,
session_id, verdict, judge_failed)`, `clear(db, session_id)`. Pure `GoalRow` helpers
(`is_running`, `budget_left`) are unit-tested without DB.

---

## Component 2 — Slash commands

In `agent/pipeline/commands.rs` (`handle_command`, which has `ctx.db` + `msg`):

- `/goal <text>` → upsert an `active` goal, reset `turn_count`, **start the driver** for
  the session, reply "Goal set. Working on it…".
- `/goal status` → report goal text, status, `turn_count/max_turns`, subgoals.
- `/goal pause` → `set_status(paused)`, stop the driver, reply.
- `/goal resume` → `set_status(active)`, start the driver, reply.
- `/goal clear` → `clear`, stop the driver, reply.
- `/subgoal <text>` → append to `subgoals`; `/subgoal list`; `/subgoal remove <n>` (1-based).

Pure parsers: `parse_goal_command(arg) -> GoalCmd` (`Set(text) | Status | Pause | Resume
| Clear`) and `parse_subgoal_command(arg) -> SubgoalCmd` (`Add(text) | List | Remove(n)`),
unit-tested without DB. To start/stop the driver, the slash `CommandContext` is extended
with read access to `cfg.agent_map` and `cfg.goal_pool` (both live on `AgentConfig`); the
handler resolves its own `Arc<AgentEngine>` from `agent_map.get(agent_name)` and calls
`goal_pool.start(arc, session_id)` / `goal_pool.stop(session_id)` (Component 3).

---

## Component 3 — Goal driver (`agent/goal/`)

**`GoalDriverPool`** stored on **`AgentConfig`** (alongside the existing
`session_pools` and `agent_map` — NOT a separate `AppState`, which the slash-command
path cannot reach): `DashMap<Uuid, GoalDriverHandle>` where `GoalDriverHandle` holds a
`tokio_util::sync::CancellationToken` + the `JoinHandle`. API: `start(engine:
Arc<AgentEngine>, session_id)`, `stop(session_id)`, `is_running(session_id)`.

**Engine acquisition (verified against `session_agent_pool.rs`):** the driver needs an
owned `Arc<AgentEngine>` to outlive the request that starts it — exactly like
`spawn_live_agent(engine: Arc<AgentEngine>, …)`. The Arc comes from the existing
**`cfg.agent_map`** registry (`agent_map.get(agent_name)`), the same source the `agent`
tool uses to spawn `LiveAgent`s. So `/goal`'s handler resolves
`cfg.agent_map.get(agent_name)` → `Arc<AgentEngine>` and calls `goal_pool.start(arc,
session_id)`, which `tokio::spawn`s `run_goal_driver(arc, session_id, cancel)`.

**Driver task** (`agent/goal/driver.rs::run_goal_driver`):
```text
loop {
    reload GoalRow; if status != active || turn_count >= max_turns { break }
    if preempted(session) { wait_for_user_turn_to_settle(); continue }   // see Component 6
    let text = run_goal_turn(session, continuation_prompt).await?;        // Component 4
    deliver(session, &text).await;                                       // Component 5
    bump_turn(session);
    match judge(session, &text).await {                                  // Component 7
        Done    => { set_status(done); deliver "✅ Goal complete."; break }
        Continue=> { record_verdict(continue, failed=false); }
        ParseFail => { record_verdict(continue, failed=true);
                       if consecutive_judge_failures >= 3 { set_status(paused);
                       deliver "⏸ Goal paused (judge unreliable). /goal resume to retry."; break } }
    }
    if cancelled { break }
}
```
When `turn_count >= max_turns`: `set_status(paused)`, deliver "⏸ Goal hit the turn budget
({max_turns}). /goal resume to continue."

The driver is spawned with everything it needs cloned from the engine (an `Arc<AgentEngine>`
or the relevant `Arc`s) so it outlives the request that started it.

---

## Component 4 — `run_goal_turn` (engine entry)

New thin adapter on `AgentEngine` (`agent/engine/run.rs`), analogous to
`handle_isolated_via_pipeline` but **continuing an existing session**:
- `bootstrap(resume_session_id = Some(session_id), force_new_session = false)` so history
  is loaded and the turn appends to the same conversation.
- `NoopSink` (no streaming; the driver delivers the final text itself).
- `execute` + `finalize` → returns the final assistant text.
- Continuation prompt built by `goal::continuation_prompt(goal_text, &subgoals)`:
  `"[autonomous continuation] Keep working toward this goal: {goal}.\n{subgoals as ranked
  list}\nWhen the goal is fully achieved, state that explicitly. Otherwise take the next
  concrete step."` — a normal user-role message.
- Uses `BehaviourLayers::for_cron(...)` (fallback provider, auto-continue,
  session-recovery) so a single autonomous turn is as robust as a cron turn.

---

## Component 5 — Delivery

`deliver(session, text)` routes by how the session is reachable:
- **Channel session** (session row `channel` is a real channel + a resolvable chat_id):
  reuse the existing send path the `message` tool uses — `send_message` `ChannelAction`
  via `self.state().channel_router` (cf. `pipeline::channel_actions::handle_message_action`
  / `send_channel_message`), `target_channel = session channel`, `context = {chat_id}`.
  Resolve `(channel, chat_id)` once at goal start from the session's latest inbound
  message context (`messages.context`) and store it on the `GoalDriverHandle`.
- **Web session** (no channel): the turn is already persisted by `finalize`; additionally
  broadcast a `ui_event` (`{type: "goal-turn", sessionId, messageId}`) on
  `state.ui_event_tx` so an open chat view can append the new message. The chat-store gains
  a small handler for this event (the only UI change).

---

## Component 6 — Preemption (real user message wins)

When a real user message arrives for a session that has an active goal:
- The normal message turn runs (channel dispatcher / web SSE) as usual.
- The driver must not run a continuation turn concurrently. A per-session
  `tokio::sync::Mutex` (reuse `session_locks` keyed by `SessionKey`, or a goal-specific
  lock) is acquired by both the normal turn and `run_goal_turn`, so they serialize.
- A `preempt` flag (an `AtomicBool` / `Notify` on the `GoalDriverHandle`) is set when a
  real message is being processed; the driver checks it at the top of each loop iteration
  and, if set, waits until the user turn finishes (lock released) before judging again —
  so if the user's own message completed the goal, the next judge says `done`.

**Serialization mechanism (v1) — the main implementation risk.** A per-session-UUID
goal lock — `DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>` on `AgentConfig` (NOT the
channel-WS `session_locks`, which is gateway-scoped and keyed by `SessionKey`). The lock
is acquired by:
- `run_goal_turn` (always — it only runs for goal sessions), and
- the user-message entry points (`handle_with_status`, `handle_sse_inner`) **only when
  the session has an active goal** (a cheap `goal_pool.is_running(session_id)` check up
  front), so non-goal traffic pays nothing.

This guarantees a user turn and an autonomous turn never overlap on the same session, and
contended user messages take the lock between driver iterations. The driver re-reads the
`GoalRow` after each turn, so if the user's own message advanced or completed the goal,
the next judge reflects it. This per-entry-point lock acquisition is the most invasive
part of the change and the primary thing to verify in code review.

---

## Component 7 — Judge

`judge(session, last_text) -> Verdict { Done | Continue | ParseFail }`:
- Model: the agent's **judge provider** — `compaction_provider` if configured, else the
  main provider. Configurable via `[agent.goal] judge_model` (optional; defaults to the
  compaction/aux model). A new optional config field; absence = current behaviour.
- Prompt (strict, ported from Hermes): system instructs a strict judge; user message
  carries the goal, subgoals, and a bounded slice of the last reply + recent messages.
  Output must be one-line JSON `{"done": <bool>, "reason": "<one sentence>"}`.
- Parsing (`goal::parse_judge_verdict(raw) -> Verdict`): tolerant JSON extraction (strip
  fences, find first `{...}`). On empty/non-JSON output → `ParseFail`.
- **Fail-open**: any judge error (API failure, parse failure) is treated as `Continue`
  for the loop (a broken judge must never wedge the agent), but `ParseFail` increments
  `consecutive_judge_failures`; 3 in a row → auto-pause (Component 3).
- `parse_judge_verdict` is unit-tested with valid/empty/fenced/garbage inputs.

---

## File structure

- `migrations/056_session_goals.sql` (new)
- `crates/hydeclaw-core/src/db/session_goals.rs` (new) + `db/mod.rs` export
- `crates/hydeclaw-core/src/agent/goal/mod.rs` (new) — `continuation_prompt`,
  `parse_judge_verdict`, `Verdict`, `GoalCmd`/`SubgoalCmd` parsers (pure)
- `crates/hydeclaw-core/src/agent/goal/driver.rs` (new) — `run_goal_driver`, judge call,
  delivery
- `crates/hydeclaw-core/src/agent/goal/pool.rs` (new) — `GoalDriverPool`, `GoalDriverHandle`
- `crates/hydeclaw-core/src/agent/engine/run.rs` — `run_goal_turn`
- `crates/hydeclaw-core/src/agent/pipeline/commands.rs` — `/goal` + `/subgoal` arms + parsers
- `crates/hydeclaw-core/src/agent/agent_state.rs` (or `AppState`) — hold `GoalDriverPool`
- `crates/hydeclaw-core/src/config/mod.rs` — optional `[agent.goal] judge_model`, `max_turns`
- `ui/src/stores/...` — handle the `goal-turn` ui_event (append message)

## Error handling
- LLM/turn failure inside `run_goal_turn` → the loop logs, records a `Continue` verdict
  (fail-open), and retries next iteration (bounded by `max_turns`).
- Judge failure → fail-open Continue; 3 consecutive parse-fails → auto-pause.
- Delivery failure (channel disconnected) → log + continue; the turn is persisted regardless.
- Driver panic → the `JoinHandle` is observed; the pool drops the handle and the goal stays
  in its last DB status (user can `/goal resume`).

## Testing
- Unit (local, no DB): `parse_goal_command`, `parse_subgoal_command`, `parse_judge_verdict`,
  `continuation_prompt`, `GoalRow::{is_running,budget_left}`, max-turns/auto-pause decision
  logic factored into a pure `next_action(state, verdict) -> DriverAction` function.
- DB (test postgres): `session_goals` upsert/get/bump_turn/set_status/clear round-trips;
  cascade on session delete.
- Server (manual): `/goal <text>` in Telegram → agent works across multiple turns, each
  delivered to chat, stops when the judge says done or at `max_turns`; `/goal pause|resume|
  clear` work; a real user message mid-loop is handled and the loop resumes after.

## Deploy / verification
- Local: `cargo test --bin hydeclaw-core` (+ test postgres for DB); `cd ui && npm test`.
- Server: `make remote-deploy` (syncs migration 056) + UI deploy for the chat-store event;
  `make doctor`; Telegram smoke of the full loop.
