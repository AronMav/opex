# Soul — Emotion Phase 2 (Coping → behavior) — Design

**Date:** 2026-07-23
**Status:** Approved design, pre-implementation (TDD)
**Area:** `crates/opex-core/src/agent/emotion/mod.rs`, `crates/opex-core/src/agent/soul/reflection.rs`, `crates/opex-core/src/agent/knowledge_extractor.rs`, `crates/opex-core/src/config/mod.rs`
**Research:** `docs/research/2026-07-14-agent-emotion-appraisal-research.md` §5 (coping)
**Prior:** emotion v1 (`docs/superpowers/specs/2026-07-14-agent-soul-emotion-layer-v1-design.md` §7 defers "Behaviour/coping" to Phase 2 with the M4-risk flag)

## 1. Problem

Emotion v1 + render-v2 let the agent *sense, remember, and surface* affect — but
affect steers **nothing the agent does**. The research (§5, EMA/Marinier/OCC)
establishes that appraisal should drive named, control-selected **coping
strategies** that audibly influence behaviour. v1 spec §7 explicitly defers this
to Phase 2 and flags the **M4-risk**: Phase 2 will consume `agency`/
`desirability`, at which point a steered `agency=other` + negative
`desirability` becomes a behaviour-steering vector an attacker can induce via
the message stream — "must be defended then".

This is **Phase 2 v1**: the safest, highest-value behaviour channel — **coping
biases the reflection trigger** (a strong negative emotion → the agent reflects
sooner, processing the event into SELF.md). Goal-dropping / tool-policy /
access-control changes remain explicitly out of scope.

## 2. Goal & scope

For an opted-in soul+emotion agent: appraise → decide a **controlled-vocabulary
coping strategy** → use it to lower the reflection-trigger threshold (sooner
reflection on emotionally significant negative sessions) → log the decision for
audit.

**In scope:**
- Pure `decide_coping(appraised) -> CopingStrategy` (fixed enum).
- Pure `reflection_threshold_bias(appraised, coping) -> f64` (bounded threshold reduction).
- Wire the bias into `should_reflect`.
- `session_timeline` `"coping_decided"` observability event.
- `[agent.emotion] coping = true` opt-in.

**Out of scope (deferred, defended later):**
- Goal drop/pause (the headline Marinier mechanism) — this is the M4 danger
  channel; needs its own defense (operator-sanctioned goals, can't be
  attacker-dropped) before shipping.
- Coping → tool-selection / access-control / SELF.md direct writes.
- Coping rendered into the prompt as a tone directive (already covered by mood
  render; adding a second tone channel risks drift feedback).

## 3. Safety — M4-risk mitigation (the core design constraint)

The v1 tri-review M4 warning: consuming `agency`/`desirability` for behaviour
opens a steering vector. **Phase 2 v1 sidesteps it by construction:**

1. **Coping is selected from `controllability` + `valence` + `intensity` only —
   NOT `agency`, NOT `desirability`.** Controllability is the agent's own
   read on whether it can affect the situation; an attacker-induced
   `agency=other` + negative `desirability` does not move the coping needle.
2. **The only behaviour effect is a bounded reduction of the reflection
   threshold.** Worst-case abuse: an adversarial message induces one extra
   reflection cycle — a bounded LLM call whose output is `sanitize_soul_text`-
   cleaned before it touches SELF.md. No goal is dropped, no policy changed,
   no access widened.
3. **`CopingStrategy` is a fixed enum** (not free-form text); the bias is a
   pure clamped function. Nothing the LLM/attacker emits reaches the prompt
   or a decision verbatim.
4. Reflection content stays behind the existing injection barrier
   (`<<<OBSERVATIONS>>>` framing + `sanitize_soul_text` + whitelist).

## 4. Design

### 4.1 Pure coping decision — `emotion/mod.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopingStrategy {
    None,        // neutral / low intensity — nothing to cope with
    PlanAct,     // negative + high controllability → agent can act on it
    Reframe,     // negative + moderate controllability → positive reinterpretation
    Accept,      // negative + low controllability → accept the situation
    SeekSupport, // negative + low controllability + very high intensity
}

pub const COPING_INTENSITY_FLOOR: f32 = 0.3;

pub fn decide_coping(a: &AppraisedEmotion) -> CopingStrategy
```

Selection (all inputs are already clamped by `RawEmotion::normalize`):
- `intensity < 0.3` → `None`.
- `valence >= 0.0` → `None` (positive affect needs no coping).
- negative valence, by `controllability`:
  - `>= 0.66` → `PlanAct`
  - `>= 0.33` → `Reframe`
  - else + `intensity >= 0.8` → `SeekSupport`
  - else → `Accept`

`CopingStrategy::as_str()` for telemetry (mirrors `Agency::as_str`).

### 4.2 Pure reflection bias — `emotion/mod.rs`

```rust
/// How much to SUBTRACT from the reflection trigger threshold (default 150)
/// when this coping is active. 0 for None / PlanAct (acting, no extra pull to
/// reflect). Bounded: intensity ≤ 1 → max 40 (~27% of a 150 threshold).
pub fn reflection_threshold_bias(a: &AppraisedEmotion, coping: CopingStrategy) -> f64
```

- `None | PlanAct` → 0.0
- `Reframe` → `intensity * 20`
- `Accept` → `intensity * 30`
- `SeekSupport` → `intensity * 40`

### 4.3 Wire into reflection trigger — `soul/reflection.rs`

`should_reflect` gains `threshold_bias: f64`:
```
effective_threshold = (cfg.reflection_threshold - threshold_bias).max(MIN_THRESHOLD)
session_capped_sum(pairs) > effective_threshold
```
`MIN_THRESHOLD` floor (e.g. 20) prevents a bias-driven threshold from hitting ~0.
`maybe_reflect` gains `threshold_bias: f64` and forwards it.

### 4.4 Caller — `knowledge_extractor.rs`

After the existing `emotion_appraised` timeline block, before `maybe_reflect`:
```text
if soul_deps.emotion.coping:
  bias = appraised.map(|a| { c = decide_coping(a); b = reflection_threshold_bias(a, c);
                              log_event("coping_decided", {strategy, intensity, valence,
                                        controllability, agency, threshold_bias: b}); b })
                  .unwrap_or(0.0)
else 0.0
maybe_reflect(..., threshold_bias = bias)
```

### 4.5 Config — `config/mod.rs`

`EmotionConfig.coping: bool` (default false). Validation:
`coping=true` requires `enabled=true` (mirrors `render_to_prompt`). Cross-section
`emotion.enabled → soul.enabled` already enforced. Operator-only (not in the
agent detail DTO), like `drift.correct`.

## 5. Testing (TDD — pure fns first)

- **`decide_coping`:** intensity-floor → None; positive valence → None; negative
  × controllability tiers (PlanAct/Reframe/Accept/SeekSupport); agency/desirability
  do NOT affect the result (M4 regression guard).
- **`reflection_threshold_bias`:** None/PlanAct → 0; bounded at intensity=1;
  monotonic in intensity within a strategy; never negative.
- **`should_reflect` (sqlx):** same accumulated importance crosses a lowered
  threshold when a strong-negative bias is applied, but not the default.
- **Config:** `coping=true` without `enabled` errors.

## 6. Rollout

Opt-in, default off:

```toml
[agent.soul]
enabled = true
[agent.emotion]
enabled = true
coping = true   # NEW — appraisal biases reflection triggering
```

Watch `session_timeline` `coping_decided` events to tune before building the
defended goal-drop phase.
