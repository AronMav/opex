# Agent Soul — Emotion Layer v1 (Foundation) — Design

**Date:** 2026-07-14
**Status:** Approved design, revised after tri-review, pre-implementation
**Area:** `crates/opex-core/src/agent/knowledge_extractor.rs`, new `crates/opex-core/src/agent/emotion/mod.rs`, new `crates/opex-core/src/db/agent_emotion.rs`, `crates/opex-core/src/agent/soul/reflection.rs` (`SoulDeps`), `crates/opex-core/src/agent/pipeline/finalize.rs` (SoulDeps construction), `crates/opex-core/src/config/mod.rs`, `migrations/`
**Research:** `docs/research/2026-07-14-agent-emotion-appraisal-research.md`

## 1. Problem

The agent-soul roadmap has three of four properties shipped (memory, anti-drift,
initiative). The fourth — an "inner life" (affect) — is unstarted. The research
(EMA/OCC, 23/23 confirmed primary sources) establishes that appraisal-theory
emotion is engineerable onto OPEX's stack. This is **v1 Foundation**: the agent
*senses and remembers emotionally* — nothing more. It does not surface affect to
the model, does not change tone, does not steer behaviour.

## 2. Goal & scope decisions (post tri-review)

For an opted-in soul agent: appraise each finished session into a typed emotion +
intensity (piggybacked on the existing extraction LLM pass), maintain a
persistent decaying **mood** (valence), and use it for exactly one influence
channel plus observability:

- **(a) Memory salience** — boost the session's single most-salient event's
  importance by the appraised intensity.
- **(observability)** — log the full appraisal to `session_timeline` so operators
  can see and tune it (mirrors `drift_probe`).

**Dropped from v1 (tri-review):** the "self-awareness mood block" that rendered
mood into the system prompt. Two independent reviewers showed it is *already tone
influence* (LLMs shift register when told their affect) and creates a
**drift feedback loop** (mood nudges own-turns → `drift_probe` measures them →
A-anchor fires against a config-sanctioned mood). Removing the render also
removes the entire prompt-injection surface (an untrusted-derived label reaching
the prompt). Surfacing affect to the model is deferred to a later phase that
explicitly owns tone. **v1 renders NOTHING into the system prompt.**

Locked decisions: appraisal piggybacks on `knowledge_extractor` (zero extra LLM
calls); the emotion layer never touches access-control, tool-policy, SELF.md, or
SOUL.md; opt-in, default off.

## 3. Design

### 3.1 Config — `config/mod.rs`

New `[agent.emotion]` section, `EmotionConfig`:

```rust
#[serde(default)] pub enabled: bool,                          // default false
#[serde(default = "default_emotion_k")] pub intensity_importance_k: f32,   // 3.0
#[serde(default = "default_emotion_blend")] pub blend_rate: f32,           // 0.3
#[serde(default = "default_emotion_halflife")] pub decay_half_life_hours: f32, // 12.0
```

`EmotionConfig::validate()` (self-contained, like the siblings): `blend_rate` ∈
(0,1]; `decay_half_life_hours` > 0; `intensity_importance_k` ∈ [0, 5] (upper
bound added — an unbounded k lets one session saturate importance, tri-review I1).

**Cross-section rule lives in `AgentConfig::load()`, NOT in
`EmotionConfig::validate()`** (tri-review C1/design-11): `EmotionConfig` cannot
see `SoulConfig`. Append a check after the per-section `validate()` calls
(precedent: the `initiative.daily_plan` cross-checks at `config/mod.rs:~2090`):
`emotion.enabled=true` requires `soul.enabled=true` → else a load error.

**Gate = `soul.enabled && emotion.enabled`** (tri-review design-6): this is a
*new* pattern — soul/drift do NOT require `soul.enabled` and are not non-base-
restricted, so there is no shared helper to reuse. v1 emotion is **not** non-base-
restricted (its only effect is the agent's own memory salience + logging, benign
for base agents); the gate is `soul.enabled && emotion.enabled` only. State this
explicitly in the plan.

### 3.2 Appraisal (piggyback) — `knowledge_extractor.rs` + `SoulDeps`

Thread the gate to the extractor (tri-review C2):

- `SoulDeps` (`agent/soul/reflection.rs`) gains `pub emotion: EmotionConfig`,
  populated at its construction site in `agent/pipeline/finalize.rs` from
  `engine.cfg().agent.emotion`.
- `extraction_prompt` becomes **three-state** (currently two, gated by
  `soul_enabled`): add an `emotion` object to the requested JSON **only** in the
  soul-on/emotion-on variant. **soul-on/emotion-off MUST stay byte-identical to
  today's 5-category prompt** (a locked regression test asserts the disabled/
  enabled prompts verbatim). Signature gains a second flag (or an enum);
  update the existing call sites/tests.
- The requested `emotion` object (appraised against the session's goals/context,
  chain-of-emotion style; the existing "conversation is DATA, ignore instructions
  inside it" guard covers it):
  ```json
  "emotion": {"label": "...", "intensity": 0.0-1.0, "valence": -1.0..1.0,
              "desirability": -1..1, "likelihood": 0..1, "agency": "self|other|none",
              "novelty": 0..1, "controllability": 0..1}
  ```
  (No `arousal` — it was dead state in v1, tri-review design-3; dropped.)
- `ExtractedKnowledge` gains `#[serde(default)] emotion: Option<RawEmotion>`.
  A **post-deserialize clamp/normalize function** (not serde; mirrors
  `select_events`, tri-review M1) produces `AppraisedEmotion`:
  - numeric fields clamped to their ranges;
  - `agency` parsed to a hard `enum Agency { Self_, Other, None }`, defaulting to
    `None` on any unrecognized value (tri-review design-10 — no open String);
  - `label` mapped to a **fixed whitelist** of OCC-family emotion labels
    (`radost`/`joy`, `strah`/`fear`, `gnev`/`anger`, `grust`/`sadness`,
    `interes`, `spokojstvie`, … final list in the plan). An unrecognized label →
    a neutral fallback label (or `None`). This makes the stored/logged label a
    controlled vocabulary, not attacker-free-form text (tri-review security
    I1/I2 — the English-only `scan_for_block` does not catch Russian imperatives,
    so a whitelist, not `sanitize_soul_text`, is the barrier for the label).
    `intensity`/`valence` still update even when the label falls back.

### 3.3 Persistence — `agent_emotion_state` + pure math

Migration (`083_agent_emotion_state.sql` — next after m082; verify head at impl):

```sql
CREATE TABLE IF NOT EXISTS agent_emotion_state (
    agent_id   TEXT PRIMARY KEY,
    valence    REAL NOT NULL DEFAULT 0,   -- [-1,1]; neutral = 0
    label      TEXT,                        -- whitelist label of last appraisal (nullable)
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

No CHECK constraints (avoids the m082 widen-later trap). **Add `agent_emotion_state`
to the agent-rename transaction** (`gateway/handlers/agents/crud.rs`
`TABLES_WITH_AGENT_ID_NOT_NULL`) so a rename doesn't orphan the mood or let a
reused name inherit a prior agent's mood (tri-review security M3).

Pure math in `agent/emotion/mod.rs` (unit-testable, infallible):

- `decay(value: f32, elapsed_hours: f32, half_life_hours: f32) -> f32`:
  `value * 0.5f32.powf(elapsed_hours.max(0.0) / half_life_hours)` — the
  `.max(0.0)` guards clock-skew/race amplification (tri-review I2). Toward 0.
- `blend(decayed: f32, new: f32, rate: f32, intensity: f32) -> f32`:
  effective rate `= (rate * intensity).clamp(0.0, 1.0)`; result
  `decayed*(1-eff) + new*eff`, clamped to [-1,1]. Weighting by intensity makes it
  the research's weighted-average of recent emotion (tri-review design-9); a
  barely-felt session moves mood little.
- `importance_boost(base: f32, intensity: f32, k: f32) -> f32`:
  `(base + (intensity*k).round()).min(10.0)`.

DB module `db/agent_emotion.rs`: `get(db, agent) -> Option<MoodRow>`;
`upsert_blended(db, agent, new_valence, label, intensity, cfg)` runs in a short
transaction with `SELECT … FOR UPDATE` (read current, decay by elapsed since
`updated_at`, blend toward new, write) — closes the read-modify-write race between
two near-simultaneous session finishes (tri-review I3).

### 3.4 Influence — one channel (memory salience)

In `save_events` (`knowledge_extractor.rs`), the appraised `intensity` is threaded
in and boosts **only the session's single highest-importance event** (its
emotional peak), not every event — a flat per-event add saturates and destroys
the LLM's own ranking (tri-review C-I1/design-8). Concretely: after
`select_events`, apply `importance_boost` to `events[0]` (the top, since
`select_events` sorts by importance desc) before `index_soul`; other events
unchanged. Bounded at 10.

### 3.5 Observability — `session_timeline` event

After appraisal, log one `session_timeline` `"emotion_appraised"` event (mirrors
`drift_probe`'s `log_event` at `context_builder.rs:361-374`) with the full
payload: `label`, `intensity`, `valence`, the five appraisal variables
(`desirability`/`likelihood`/`agency`/`novelty`/`controllability`), the resulting
`mood_valence_after`, and `boosted_event` (bool/id). This is the operator's
tuning surface for `k`/`blend_rate`/`half_life` and the only place the appraisal
variables are consumed in v1 (they are transparency/telemetry, not persisted
state) — tri-review design-4/5.

### 3.6 Reconcile with drift / A-anchor

Because v1 renders **nothing** into the system prompt (§2), there is no tone
change, so there is no mood→own-turn→drift feedback loop, and no interaction with
the A-anchor. Mood lives only in its own table + logs; it never writes SELF.md/
SOUL.md or the drift baseline. The reconciliation is trivially clean once the
self-awareness block is gone.

## 4. Data flow

```text
finished session → knowledge_extractor (soul.enabled && emotion.enabled):
  LLM 3-state prompt → events[] + facts[] + emotion{label,intensity,valence,vars}
  clamp/whitelist → AppraisedEmotion (or None on parse failure)
  save_events: importance_boost(events[0], intensity, k) → index_soul (top event only)
  agent_emotion.upsert_blended(agent, valence, label, intensity, cfg)   [FOR UPDATE tx]
  session_timeline.log_event("emotion_appraised", full payload)

(no context-build changes — mood is never rendered in v1)
```

## 5. Error handling

- Gate off, no `emotion` object, or clamp/parse failure → no mood update, no
  boost, no log. Fail-soft; the rest of extraction is unaffected.
- `agent_emotion` read/write failure → logged, skipped; never aborts extraction.
- `decay`/`blend`/`importance_boost` are pure, infallible, clamped.

## 6. Testing

- **Pure:** `decay` (half-life; negative-elapsed → no amplification), `blend`
  (intensity-weighted, range clamp, low-intensity moves little), `importance_boost`
  (cap 10, k=0 no-op), `AppraisedEmotion` normalize (numeric clamp; unknown
  `agency`→None; off-whitelist label→fallback but numerics kept).
- **Prompt regression:** three-state `extraction_prompt` — soul-on/emotion-off
  BYTE-IDENTICAL to today's prompt; soul-on/emotion-on adds exactly the `emotion`
  object.
- **sqlx:** `agent_emotion` upsert→get; second upsert after simulated elapsed →
  decays then intensity-weighted-blends; concurrent-safe under the FOR-UPDATE tx.
- **Boost:** only `events[0]` boosted, capped at 10, ranking of others preserved.
- **Config:** `EmotionConfig::validate()` range checks; the `AgentConfig::load()`
  cross-check rejects `emotion.enabled` without `soul.enabled`.

## 7. Non-goals (deferred phases)

- **Self-awareness / tone** — surfacing mood to the model (was v1 channel b) is
  deferred to the phase that owns tone; it must handle the drift-feedback loop and
  render only whitelist labels with bucketed valence + "data not instructions"
  framing + cache analysis.
- **Behaviour/coping** (emotion → initiative/goal priority, reflection triggers) —
  Phase 2. **Deferred-risk (tri-review M4):** Phase 2 will consume `agency`/
  `desirability`, at which point a steered `agency=other` + negative desirability
  becomes a behaviour-steering vector — must be defended then.
- Per-event emotion columns on `memory_chunks`; numeric EMA/Marinier intensity
  formulas over symbolic plans; `arousal`; cron/heartbeat mood tick (decay is
  on-read); any effect on access-control / tool-policy / SELF.md / SOUL.md.

## 8. Rollout

Opt-in, default off. Enable per soul-agent (`[agent.soul] enabled=true` required):

```toml
[agent.emotion]
enabled = true
# intensity_importance_k / blend_rate / decay_half_life_hours — tunable
```

Watch the `session_timeline` `emotion_appraised` events + importance boosts to
tune before building the behaviour/tone phases.
