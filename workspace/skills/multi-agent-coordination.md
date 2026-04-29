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

The `agent` tool is your single way to talk to peer agents. It exposes
exactly three actions.

| Action   | What it does                                                                                |
| -------- | ------------------------------------------------------------------------------------------- |
| `ask`    | Send `text` to `target` and **block** for the answer. Auto-spawns the peer if it is idle, continues the existing dialog if it is alive. Always synchronous. |
| `status` | Inspect what peers are doing (one or all). Non-blocking. Used only for debugging.            |
| `kill`   | Free a peer's slot in the session pool.                                                      |

Parameters:

- `target` — name of the peer (required for `ask` and `kill`, optional for `status`).
- `text` — message / task text (required for `ask`).
- `fresh` — `true` to kill any existing instance of `target` before asking. Default: `false`.

Timeouts are layered: the inner caps are `message_wait_for_idle_secs`
(default 60 s, applies only on the continue-dialog path) plus
`message_result_secs` (default 300 s) — both configurable. An outer
`safety_timeout_secs` (default 600 s) wraps every `agent` call as
defense-in-depth. Under normal conditions the inner caps fire first.

Agents work in isolated contexts — they do not see your conversation
history. Send only the data they need.

```text
Task: [specific description]
Context: [minimum needed data]
Response format: [what to return]
```

---

## Recommended Patterns

### 1. One-shot delegation

```text
agent(action="ask", target="Alma", text="Analyze portfolio risk and return a summary table")
→ blocks → returns Alma's analysis directly
```

The peer **stays alive** in the pool after the answer. If you do not
expect any follow-ups, free the slot:

```text
agent(action="kill", target="Alma")
```

### 2. Multi-turn dialog

Just call `ask` again with the same `target`. Prior context is preserved.

```text
agent(action="ask", target="Alma", text="Analyze Q1 revenue")
→ blocks → first reply

agent(action="ask", target="Alma", text="Now compare with Q2")
→ blocks → reply that builds on the previous turn

// when done
agent(action="kill", target="Alma")
```

Set `fresh=true` if you specifically need to discard the prior context
and start over. This is destructive — use deliberately.

### 3. Parallel fan-out (rare)

When two or more peers can work simultaneously, emit multiple `ask`
calls in **one tool batch**. The engine runs them concurrently:

```text
// One tool-call batch from your turn:
agent(action="ask", target="Alma", text="Analyze Q1 revenue")
agent(action="ask", target="Hyde", text="Fetch latest market news")
```

You receive both results when both peers finish. Skip this pattern
unless you genuinely have ≥ 2 independent peers; for one delegation
just use Pattern 1.

---

## Anti-Patterns (don't do these)

- **Polling `status` in a loop** — `ask` already blocks server-side. No LLM-side polling.
- **Calling `status` between consecutive `ask` calls to the same peer** — `ask` automatically waits for the peer to be idle (up to ~60 s).
- **Using `fresh=true` "just in case"** — it loses prior context. Default is correct in 99% of cases.
- **Sending the entire conversation as `text`** — peers don't share your history; send only the slice they need.
- **Delegating trivial tasks** — if you can answer in one tool call, just do it.
- **Leaving long chains of peers alive** — each holds a slot in the session pool. End sessions with `kill` if you do not expect follow-ups.

---

## Rules

- `ask` **blocks** — no polling needed.
- Peers take 1–5 minutes on this hardware — that is normal, not a hang.
- Send minimum context — peers do not see your chat history.
- End the dialog with `kill` if you do not expect to return to a peer.
- Don't create plan files for single-agent tasks — overhead isn't worth it.
