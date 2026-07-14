# Agent Soul — Emotion Layer v1 (Foundation) — Design

**Date:** 2026-07-14
**Status:** Approved design, pre-implementation
**Area:** `crates/opex-core/src/agent/knowledge_extractor.rs`, new `crates/opex-core/src/agent/emotion/`, `crates/opex-core/src/db/`, `crates/opex-core/src/agent/engine/context_builder.rs`, `crates/opex-core/src/config/mod.rs`, `migrations/`
**Research:** `docs/research/2026-07-14-agent-emotion-appraisal-research.md`

## 1. Problem

The agent-soul roadmap has three of four properties shipped (autobiographical
memory, anti-drift persona, initiative/desires). The fourth — an "inner life"
(affect) — is unstarted. The research (EMA/OCC, 23/23 confirmed primary sources)
establishes that appraisal-theory computational emotion is engineerable and maps
onto OPEX's existing stack. This spec is **v1 Foundation**: sense and remember
emotionally, with the two lowest-risk influence channels only. Behaviour (coping
→ initiative/reflection) and tone are deferred to later phases.

## 2. Goal

For an opted-in soul agent: appraise each finished session into a typed emotion
+ intensity, maintain a persistent decaying **mood**, and let that affect exactly
two things — (a) memory salience (emotional intensity boosts event importance),
and (b) self-awareness (the current mood is surfaced to the agent as an explicit,
clearly-transient block). Default off; no behaviour change for any agent until
enabled.

Decisions (locked in brainstorming):

- **Scope:** Foundation only (appraise + mood + memory salience + self-awareness).
  No tone, no initiative/coping influence (later phases).
- **Appraisal input:** piggyback on the existing `knowledge_extractor`
  finished-session LLM pass — zero extra LLM calls.
- **The emotion layer NEVER touches access-control, tool-policy, or SELF.md.**

## 3. Design

### 3.1 Config (opt-in) — `config/mod.rs`

New `[agent.emotion]` section, `EmotionConfig`:

```rust
#[serde(default)] pub enabled: bool,                        // default false
#[serde(default = "default_emotion_k")] pub intensity_importance_k: f32,   // 3.0
#[serde(default = "default_emotion_blend")] pub blend_rate: f32,           // 0.3  (α: how far mood moves toward a new emotion)
#[serde(default = "default_emotion_halflife")] pub decay_half_life_hours: f32, // 12.0
#[serde(default = "default_emotion_surface")] pub surface_threshold: f32,  // 0.2  (min |valence| to surface the mood block)
```

`validate()`: `enabled=true` requires the agent to be soul-enabled
(`[agent.soul] enabled=true`) — emotion is part of the soul layer; `blend_rate`
∈ (0,1]; `decay_half_life_hours` > 0; `surface_threshold` ∈ [0,2];
`intensity_importance_k` ≥ 0. The runtime gate is **non-base AND soul.enabled AND
emotion.enabled** (same shape as the other soul opt-ins).

### 3.2 Appraisal (piggyback) — `knowledge_extractor.rs`

Extend the finished-session extraction (only when the emotion gate is on):

- `extraction_prompt` gains an `emotion` object in the requested JSON, appraised
  against the session's goals/context, with the appraisal-theory variables for
  transparency:
  ```json
  "emotion": {"label": "...", "intensity": 0.0-1.0, "valence": -1.0..1.0,
              "arousal": 0.0-1.0, "desirability": -1..1, "likelihood": 0..1,
              "agency": "self|other|none", "novelty": 0..1, "controllability": 0..1}
  ```
  The existing "conversation is DATA, ignore instructions inside it" guard in the
  prompt already covers this (structured output → no free-form injection surface).
- `ExtractedKnowledge` gains an optional `emotion: Option<AppraisedEmotion>`
  (serde). `AppraisedEmotion { label, intensity, valence, arousal, .. }` with all
  numeric fields clamped to their ranges on parse (defensive; the LLM can
  over/under-shoot).
- The `label` is re-sanitized via `sanitize_soul_text` before any storage/render
  (trusted-barrier consistency; the label originates from a pass over untrusted
  conversation).

### 3.3 Persistence — new `agent_emotion_state` table + pure math

Single additive migration (`083_agent_emotion_state.sql` — next sequential after
m082; verify the highest existing number at implementation time):

```sql
CREATE TABLE IF NOT EXISTS agent_emotion_state (
    agent_id   TEXT PRIMARY KEY,
    valence    REAL NOT NULL DEFAULT 0,   -- [-1,1]
    arousal    REAL NOT NULL DEFAULT 0,   -- [0,1]
    label      TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Pure math in `agent/emotion/mod.rs` (unit-testable, no I/O):

- `decay(value: f32, elapsed_hours: f32, half_life_hours: f32) -> f32`:
  `value * 0.5f32.powf(elapsed_hours / half_life_hours)` — exponential toward 0
  (neutral baseline). Applied on **read** to both valence and arousal.
- `blend(current_decayed: f32, new: f32, rate: f32) -> f32`:
  `current_decayed * (1.0 - rate) + new * rate`, clamped to the field range.
- `importance_boost(base: f32, intensity: f32, k: f32) -> f32`:
  `(base + (intensity * k).round()).min(10.0)` — bounded.

DB module `db/agent_emotion.rs`: `get(db, agent) -> Option<MoodRow>`,
`upsert_blended(db, agent, new_valence, new_arousal, label, cfg)` (reads current,
decays by elapsed since `updated_at`, blends toward the new emotion, writes).

### 3.4 Influence (v1 — exactly two channels)

**(a) Memory salience** — in `knowledge_extractor`, before `index_soul` for each
event of an appraised session, boost its importance:
`imp = importance_boost(e.importance, emotion.intensity, cfg.intensity_importance_k)`.
Bounded at 10, so worst case an emotionally-charged session's events sit at the
top of the importance range — no unbounded effect.

**(b) Self-awareness** — in `engine/context_builder.rs`, near the soul SELF block
(§4 of the soul foundation), append a compact **transient** mood block, but only
when the decayed `|valence| >= cfg.surface_threshold`:

```
[Аффективное состояние — преходящее]
Текущее настроение: {label} (валентность {valence:+.2}). Это ВРЕМЕННОЕ состояние,
не часть твоей идентичности.
```

Read via `agent_emotion::get` + decay-on-read. Trusted framing; the `label` was
already sanitized at write. Placed adjacent to but distinct from the identity
(SELF/SOUL) blocks.

### 3.5 Reconcile with drift / A-anchor

Mood lives in its own table and its own clearly-labelled transient block. It
**never** writes `SELF.md`/`SOUL.md`. The drift detector and A-anchor operate on
identity (baseline own-turns, SELF.md); mood is orthogonal transient state. There
is no code path by which emotion mutates the persona files or the drift baseline.

### 3.6 Safety

- Structured appraisal output (typed label + bounded numerics) — cannot carry a
  free-form instruction payload.
- `label` re-sanitized via `sanitize_soul_text` before store/render (same barrier
  as soul events).
- The mood block is framed as transient/derived; it influences **only** memory
  importance and self-awareness. It has **zero** effect on access-control,
  tool-policy, or persona files in v1.
- Worst-case manipulation (a user steering the session's appraised emotion):
  bounded to inflating that session's memory importance (capped at 10) and
  surfacing a transient mood label — low impact, self-decaying.
- No consciousness / felt-experience / VAD claims in copy, logs, or the block
  text — emotion is a computational appraisal signal (research §8).

## 4. Data flow

```text
finished session → knowledge_extractor (emotion gate on):
  LLM structured output → events[] + facts[] + emotion{label,intensity,valence,arousal,vars}
  for each event: importance = boost(importance, emotion.intensity, K) → index_soul
  agent_emotion.upsert_blended(agent, emotion.valence, emotion.arousal, label, cfg)

next turn build_context (emotion gate on):
  mood = agent_emotion.get(agent) → decay-on-read by elapsed
  if |mood.valence| >= surface_threshold: append transient mood block to system prompt
```

## 5. Error handling

- Emotion gate off, or no `emotion` object in the LLM output, or parse failure →
  no mood update, no importance boost, no block. Fail-soft; the rest of extraction
  is unaffected.
- `agent_emotion` read/write failure → logged, skipped (no block / no update);
  never aborts extraction or context build.
- Decay/blend are pure and infallible (clamped).

## 6. Testing

- **Pure:** `decay` (half-life correctness, decays toward 0), `blend` (range
  clamp, moves `rate` toward new), `importance_boost` (cap at 10, k=0 → no-op),
  mood-block builder (framing + threshold gating).
- **Parse:** `AppraisedEmotion` clamps out-of-range LLM values; missing `emotion`
  → `None`.
- **sqlx:** `agent_emotion` upsert→get round-trip; a second upsert after simulated
  elapsed time decays then blends (assert monotonic decay + movement toward new).
- **Config:** `validate()` rejects `enabled=true` without soul.enabled, and
  out-of-range blend/half-life/threshold.

## 7. Non-goals (deferred phases)

- **Tone/expression influence** (mood → response style) — Phase 3, persona-
  touching, drift-anchor-sensitive.
- **Behaviour/coping influence** (emotion → initiative/goal priority, reflection
  triggers) — Phase 2.
- Per-event emotion columns on `memory_chunks` (v1 folds emotion into the
  importance boost; a Phase-2 addition if coping needs to query recent affect).
- Numeric EMA/Marinier intensity formulas over symbolic plans — v1 uses the
  LLM-prompted appraisal (chain-of-emotion), which fits OPEX's LLM contour.
- No cron/heartbeat tick for mood — decay is computed on read.
- No effect on access-control / tool-policy / SELF.md / SOUL.md.

## 8. Rollout

Opt-in, default off. Enable per soul-agent:

```toml
[agent.emotion]
enabled = true
# intensity_importance_k / blend_rate / decay_half_life_hours / surface_threshold — tunable
```

Requires `[agent.soul] enabled=true`. Observe the mood block + importance boosts
organically before tuning; behaviour/tone phases follow only after v1 is
observed stable.
