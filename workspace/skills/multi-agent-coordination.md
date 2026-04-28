---
name: multi-agent-coordination
description: Task coordination between agents — delegation, parallel execution, multi-turn dialog
triggers:
  - delegate
  - assign to agent
  - ask agent
  - coordination
  - spawn agent
  - orchestrate
  - coordinate agents
  - run in parallel
  - multi-step task
  - task plan
  - делегируй
  - поручи агенту
  - спроси агента
  - координация
  - параллельная задача
  - план задачи
priority: 5
tools_required:
  - agent
---

## Agent Tool — Reference

The `agent` tool delegates tasks to other agents and manages their lifecycle.
**Synchronous calls are the default** — you don't need to poll status loops.

### Actions

| Action    | Default behavior                                                                              |
| --------- | --------------------------------------------------------------------------------------------- |
| `run`     | Spawn a peer agent and **block** until it finishes (~5 min cap), then auto-clean up.          |
| `message` | Send a follow-up to a live agent and **block** until it returns its result (~6 min cap).      |
| `status`  | Read state of one or all live agents (used only for inspection / fan-out polling).            |
| `kill`    | Terminate a live agent and free its slot.                                                     |
| `collect` | Block on an `async`-spawned agent until it completes (only needed after `run` with async).    |

Timeouts are layered: inner caps (sync `run` 300s, sync `message` 360s)
fire first; an outer 600s safety-net wraps every `agent` call as
defense-in-depth.

### Parameters

- `target` — name of the peer agent (required for run/message/kill/collect).
- `task` — initial task text (run only).
- `text` — message text (message only).
- `mode` — `"sync"` (default) or `"async"` for `run`. Async returns immediately; pair with `collect` or `message`.
- `wait_for_result` — `true` (default) or `false` for `message`. False = fire-and-forget after delivery.

Agents work in isolated contexts — they do not see your conversation history. Send only the data they need.

```text
Task: [specific description]
Context: [minimum needed data]
Response format: [what to return]
```

---

## Recommended Patterns

### 1. Single delegated task (sync run)

```text
agent(action="run", target="Alma", task="Analyze portfolio risk and return a summary table")
→ blocks 1–5 min → returns Alma's analysis directly
→ Alma is auto-removed from the session pool when done
```

Use this whenever the answer is a one-shot delegation. No follow-ups, no cleanup.

### 2. Multi-turn dialog (async run + sync messages)

When you need ongoing collaboration with the same peer, keep them alive:

```text
// 1. Spawn without blocking — short initial task
agent(action="run", target="Alma", mode="async", task="Initialize portfolio analysis context")

// 2. Each message blocks until Alma finishes processing it
agent(action="message", target="Alma", text="What's the risk for tech sector?")
→ blocks → Alma's reply

agent(action="message", target="Alma", text="Now compare with healthcare")
→ blocks → updated reply

// 3. Clean up explicitly
agent(action="kill", target="Alma")
```

Sync `message` automatically waits for the target to become idle (up to 60s) before delivering — no need to manually poll status.

### 3. Parallel fan-out (rare)

When two or more peers can work simultaneously and you want to interleave:

```text
agent(action="run", target="Alma", mode="async", task="Analyze Q1 revenue")
agent(action="run", target="Hyde", mode="async", task="Fetch latest market news")

// Block on each result in turn
agent(action="collect", target="Alma")
agent(action="collect", target="Hyde")
```

Or, if you also need follow-up after the initial run, swap `collect` for `message(wait_for_result=true)` — both are valid. Skip this pattern for ≤1 peer.

---

## Anti-Patterns (don't do these)

- **Polling `status` in a loop** — sync `run` and sync `message` already block server-side. No LLM-side polling needed.
- **Sending `message` immediately after async `run` and giving up on a "queue full" error** — sync `message` waits for idle automatically (up to 60s).
- **Using async mode for one-shot delegations** — sync `run` is simpler, auto-cleans the agent, and is what you want by default.
- **Sending the entire conversation as `task`/`text`** — peers don't share your history; send only the slice they need.
- **Delegating trivial tasks** — if you can answer in one tool call, just do it.

---

## Rules

- Default `run` and `message` **block** — no polling needed.
- Agents take 1–5 minutes on this hardware — that is normal, not a hang.
- Only use `mode="async"` when you have a clear reason (multi-turn dialog or parallel fan-out).
- Always `kill` async agents when the dialog is done — they hold a slot in the session pool.
- Send minimum context — peers don't see your chat history.
- Don't create plan files for single-agent tasks — overhead isn't worth it.
