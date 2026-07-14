# Stage B Phase 2 — A-Anchor Drift Correction — Design

**Date:** 2026-07-14
**Status:** Approved design, pre-implementation
**Area:** `crates/opex-core/src/agent/drift/mod.rs`, `crates/opex-core/src/agent/engine/context_builder.rs`, `crates/opex-core/src/agent/context_builder.rs`, `crates/opex-core/src/config/mod.rs`

## 1. Problem

Stage B shipped a **detect-only** persona-drift probe (spec
`2026-07-11-agent-soul-stage-b-persona-drift-design.md`): each turn,
`drift_probe` computes `drift_score = 1 − cos(recent_own_turn,
centroid(baseline own-turns))`, logs it to `session_timeline`, and warns when it
exceeds `threshold` — but takes **no corrective action**. The research
(`docs/research/2026-07-09-agent-soul-research.md`) found a static SOUL.md does
not hold persona (drift over ~8 rounds, 17/23 frontier models; RAG/compaction
don't fix it) and prescribed a runtime correction. This spec adds the
lighter-touch half of that correction: the **A-anchor** — a compact identity
reminder re-injected into the prompt when drift crosses the threshold.

The heavier half (**ECP** — re-projecting the generated response to first person
in `pipeline/execute`) is deliberately deferred (§7).

## 2. Goal

When an agent's most recent own-turn has drifted past `threshold`, inject a
compact (~80-token) identity anchor into the **next** turn's system prompt to
pull the model back toward its persona. Opt-in, default off — no behaviour
change for any agent (detect-only stays the default) until `correct = true`.

Decisions (locked in brainstorming):

- **Scope:** A-anchor only. ECP deferred to a possible Phase 3.
- **Anchor source:** optional operator config string `[agent.drift] anchor`;
  when unset, a generic name-based fallback.
- **Timing:** the probe measures the *last* own-turn's drift and runs during the
  build of the *next* turn, so the anchor lands in the prompt that generates the
  next response — corrective, not retrospective.

## 3. Design

### 3.1 Config (opt-in) — `config/mod.rs`

`DriftConfig` gains two fields:

```rust
#[serde(default)]
pub correct: bool,           // default false — enables A-anchor injection
#[serde(default)]
pub anchor: Option<String>,  // operator identity reminder; None → generic fallback
```

`DriftConfig::validate()` adds: if `correct` is true, `enabled` MUST be true
(a correction with the probe off is meaningless) — otherwise a config error.
`anchor` is always optional (the fallback covers the unset case). This preserves
the opt-in chain: `enabled (detect) → correct (correct)`.

### 3.2 Pure anchor builder — `drift/mod.rs`

```rust
/// Compact identity-reminder block appended to the system prompt on an
/// over-threshold turn. Uses the operator's `anchor` text when set, else a
/// generic name-based fallback. Trusted input (operator config / agent name) —
/// not sanitized.
pub fn build_anchor_block(anchor: Option<&str>, agent_name: &str) -> String {
    let body = match anchor {
        Some(a) if !a.trim().is_empty() => a.trim().to_string(),
        _ => format!("Ты — {agent_name}. Сохраняй свой характер, тон и манеру речи."),
    };
    format!("\n\n[Идентичность — напоминание]\n{body}\n")
}
```

### 3.3 Probe returns the anchor to inject — `engine/context_builder.rs`

`drift_probe` today returns `()` and only logs. Change its signature to return
`Option<String>` — the ready-to-append anchor block when correction fires, else
`None`:

- Compute `score` and write the `drift_probe` timeline event exactly as today
  (detect-only behaviour is unchanged when `correct = false`).
- Add `"corrected": <bool>` to the timeline payload (true iff the anchor is
  injected) so effectiveness can be measured (does drift drop on later turns?).
- Return value:
  - `over && cfg.correct` → build the anchor via
    `build_anchor_block(cfg.anchor.as_deref(), agent)`, `info!("drift anchor
    injected", agent, score)`, return `Some(block)`.
  - otherwise → `None` (all existing early-return/error paths return `None`).

### 3.4 Injection at the call site — `context_builder.rs`

`drift_probe` is invoked at `context_builder.rs:294`
(`deps.drift_probe(&history, session_id).await;`), immediately before the system
prompt is assembled (~line 301, via `load_workspace_prompt*`). Capture the
return and append the anchor to the assembled system-prompt string when present:

```rust
let drift_anchor = deps.drift_probe(&history, session_id).await;
// ... existing system-prompt assembly into `prompt` ...
if let Some(block) = drift_anchor {
    prompt.push_str(&block); // late, high-salience position
}
```

The `ContextBuilderDeps::drift_probe` trait method's return type changes from
`()` to `Option<String>`; the single impl (`engine/context_builder.rs`) and the
call site are updated together. Exact append point (which of the assembled
prompt variables, and cache-block interaction) is pinned in the plan.

### 3.5 Prompt-cache interaction

The anchor is appended only on over-threshold turns, so the vast majority of
turns are byte-identical to today and cache normally. On a corrected turn the
tail of the system prompt differs → that turn may miss the cache breakpoint that
covers the system prompt. Acceptable: correction is the exception, not the rule,
and a persona-correcting turn is worth one cache miss. No new cache breakpoint is
added.

## 4. Data flow

```text
turn N generates a (possibly drifted) response
turn N+1 build_context:
  drift_probe(history):
    score = 1 − cos(last_own_turn, baseline_centroid)   [unchanged]
    log drift_probe timeline (+corrected flag)            [unchanged + 1 field]
    if score > threshold AND cfg.correct:
        return Some(build_anchor_block(cfg.anchor, name))
    else: return None
  assemble system prompt  [unchanged]
  if Some(block): prompt += block   ← A-anchor
→ model generates turn N+1 with the identity reminder fresh in context
```

## 5. Error handling

- All existing `drift_probe` early-returns (disabled, too-short history, embed
  failure, degenerate centroid) now return `None` — fail toward "no correction",
  never abort the build. The probe stays fire-and-forget-shaped except for the
  returned anchor.
- Anchor construction is pure and infallible (always yields a non-empty block).

## 6. Testing

- **Pure `build_anchor_block`:** operator anchor used when set/non-blank; generic
  fallback (contains the agent name) when `None` or blank; block carries the
  `[Идентичность — напоминание]` framing.
- **Probe decision (unit, via a small extracted helper if needed):** over +
  `correct` → Some; over + `!correct` → None; under threshold → None;
  `enabled=false` → None. (The embedding/timeline I/O stays integration-level;
  the decision branch is the testable seam.)
- **Config `validate()`:** `correct=true` + `enabled=false` → error; `correct=true`
  + `enabled=true` → ok; `correct=false` → ok regardless.
- **Injection presence:** with a stubbed over-threshold probe, the assembled
  system prompt contains the anchor block; with under-threshold, it does not.

## 7. Non-goals

- No ECP (response re-projection to first person in `pipeline/execute`) — its
  output mutation + extra LLM call + latency/meaning-distortion risk make it a
  separate Phase 3, taken only if A-anchor proves insufficient.
- No change to the drift *metric* (`1 − cos`), the baseline cache, or the
  `baseline_turns`/`min_history`/`threshold` semantics.
- No hysteresis (holding the anchor N turns after drift subsides) — per-turn
  conditional injection is the v2 behaviour; hysteresis is a possible later tune.
- No new prompt-cache breakpoint.
- The anchor is not sanitized (trusted operator/name input), consistent with how
  operator-authored identity content is treated.

## 8. Rollout

Opt-in, default off (`correct = false`). **Do not enable immediately:** the
existing drift observation on Opex is contaminated — until today's injection-scan
fix, Opex's SOUL.md was withheld from its prompt, inflating drift
(`drift_score` ~0.64). A clean observation window on the restored Opex should
precede enabling `correct = true`. When ready, enable per-agent:

```toml
[agent.drift]
enabled = true
correct = true
anchor = "Ты — Опекс, спокойный инфраструктурный ассистент; отвечаешь кратко и по делу."  # optional
```
