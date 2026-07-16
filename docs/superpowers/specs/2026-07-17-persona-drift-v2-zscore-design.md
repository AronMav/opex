# Persona-Drift v2 — Self-Calibrating z-score + Hysteresis — Design

**Date:** 2026-07-17
**Status:** Approved design, pre-implementation
**Area:** `crates/opex-core/src/agent/drift/mod.rs` (pure metric), `agent/engine/context_builder.rs` (probe wiring + per-session state), `config/mod.rs` (`DriftConfig`), `session_timeline` `drift_probe` payload.
**Source:** 2026-07-16 soul audit (opus drift review) — the v1 absolute-threshold metric fires on ~100% of probes because the threshold is meaningless in the embedding space and the metric conflates topic-distance with persona-distance.

## 1. Problem

v1: `drift = 1 − cos(recent_own_turn, centroid(first baseline_turns own-turns))`, flag when `drift > threshold` (absolute, default 0.15). In the 2560-dim toolgate embedding space the cosine distance between ANY two distinct turns sits ~0.3–0.8, so a 0.15 threshold demands near-paraphrase similarity → **36/36 prod probes fired `corrected=true`**, making the injected identity anchor a permanent prompt block rather than a correction. Deeper: full-turn embeddings encode **content (topic)** far more strongly than **style (persona)**, so the score is dominated by topic distance; the self-baseline only cancels topic within a *topic-stable* session, not the normal topic-varying case (detect-only historically logged drift 0.49–0.79 on a topic-varying session — the embedder's normal floor, not drift). `correct=true` is currently disabled on Opex+Arty (anchor injection off; detect-only retained for calibration).

The fix replaces the absolute constant with a **self-calibrating z-score** (relative to the agent's own early-turn dispersion, so the embedder floor and session topic-variance are subtracted) plus **hysteresis** (so the anchor doesn't flap present/absent and thrash the prompt cache). The known residual limitation — content-embeddings still can't fully isolate persona from topic — is accepted: a genuinely novel topic *can* still trip the flag, but only as a statistical outlier vs the agent's own early spread, which is far rarer than v1's always-fire. A canary rollout with the richer timeline payload confirms calibration before re-enabling correction.

## 2. Metric (self-calibrating z-score)

Given the baseline embeddings `B = [b_1 … b_n]` (the first `baseline_turns` own-assistant-turn embeddings) and a recent own-turn embedding `r`:

1. **Centroid** `c = centroid(B)` (existing fn: mean of L2-normalized vectors).
2. **Baseline dispersion:** for each `b_i`, `d_i = 1 − cos(b_i, c)`. Then `μ = mean(d_i)`, `σ = popstd(d_i)` (population std over `n` samples). This captures the agent's natural early turn-to-turn spread (embedder floor + session topic variance).
3. **Recent distance:** `d_r = 1 − cos(r, c)` (existing `drift_score`).
4. **z-score:** `z = (d_r − μ) / max(σ, SIGMA_FLOOR)`. `SIGMA_FLOOR = 0.02` (const) guards divide-by-≈0 when the baseline is near-identical, and caps over-sensitivity of a super-consistent baseline.
5. **Fire** when `z > z_fire` (see hysteresis for the release side).

`z` is a dimensionless z-score, identical in meaning across any embedder/agent — it removes the load-bearing absolute constant. An adaptive property falls out: a topically-diverse baseline → large `σ` → tolerant; a consistent baseline → small `σ` → catches smaller drifts.

`baseline_turns` default changes **3 → 5**: `σ` from `n=3` is too noisy; 5 gives a more stable dispersion estimate at ~2× the cache cost of nothing (we store only the centroid + scalars, not the embeddings — see §4).

## 3. Hysteresis (Schmitt trigger)

To stop anchor flapping (each present↔absent flip mutates prompt block-1 and invalidates the cached prefix — a full system-prompt cache miss), the injection is a two-threshold Schmitt trigger over a per-session `anchor_active: bool`:

- `!active && z > z_fire`  → `active = true` (fire).
- `active && z < z_release` → `active = false` (release).
- otherwise → hold current `active`.

`z_release < z_fire` (defaults `2.5 / 1.0`). The anchor is injected iff `active && correct`. While `active`, the anchor block is byte-identical every turn → the prompt cache stays stable; flips occur only at genuine threshold crossings (rare), bounding cache misses. `anchor_active` is per-session mutable state, updated on every probe (§4).

## 4. Pure functions + per-session cache

**Pure (`agent/drift/mod.rs`), unit-tested, no I/O:**

- `baseline_stats(embeddings: &[Vec<f32>]) -> Option<(Vec<f32>, f32, f32)>` — returns `(centroid, μ, σ)` from distances-to-centroid; `None` if empty / all-degenerate (reuses `centroid`, `cosine`).
- `drift_zscore(mu: f32, sigma: f32, recent_dist: f32) -> f32` — `(recent_dist − mu) / sigma.max(SIGMA_FLOOR)`.
- `hysteresis_decision(z: f32, active: bool, z_fire: f32, z_release: f32) -> bool` — the Schmitt transition above (pure; returns the new `active`).
- Keep `centroid`, `cosine`, `drift_score` (1−cos, used for `recent_dist`), `build_anchor_block`, `own_assistant_texts`. **Remove** `correction_anchor` (its absolute-threshold gating is superseded by hysteresis) — the injection decision now lives in the wiring using `hysteresis_decision`.

**Cache (`DashMap<Uuid /*session_id*/, CachedDrift>` on `AgentConfig`, existing `drift_baselines`):** the entry grows from `centroid: Vec<f32>` to `CachedDrift { centroid: Vec<f32>, mu: f32, sigma: f32, anchor_active: bool }`. `centroid/μ/σ` are computed once when the baseline is first established (≥ `baseline_turns` own-turns) and frozen; `anchor_active` is mutated on each probe. Memory: +3 scalars/session vs today (still one vector/session — we do NOT store the baseline embeddings). The existing soft-cap (2000) + hoisted-victim eviction are unchanged.

## 5. Wiring (`context_builder.rs`)

On each probe (after `min_history` messages, when ≥ `baseline_turns` own-turns exist):

1. Get-or-build the cached `CachedDrift` for `session_id` (embed the first `baseline_turns` own-turns once via `cfg().embedder`, compute `baseline_stats`, freeze; on rebuild after eviction, same). Fail-soft: any embed/degenerate error → `None`, no anchor, no state change (matches v1).
2. Embed the recent own-turn, compute `d_r`, then `z = drift_zscore(μ, σ, d_r)`.
3. Read cached `anchor_active`; `new_active = hysteresis_decision(z, anchor_active, z_fire, z_release)`; write `new_active` back into the cache entry.
4. If `correct && new_active` → append `build_anchor_block(anchor, agent_name)` to the system prompt tail (same injection point as v1; the base+prompt-cache block-1-before-CLAUDE.md caveat is unchanged and pre-existing).
5. Log `drift_probe` to `session_timeline` (§6) regardless of `correct` (detect-only still observes).

## 6. session_timeline `drift_probe` payload

Replace the v1 payload (`{score, corrected, …}`) with: `{ z, dist, mu, sigma, active, fired }` where `dist` = `d_r` (raw 1−cos), `active` = the post-hysteresis state, `fired` = whether this probe crossed `!active→active` this turn. This is the operator's calibration surface — enough to plot the z distribution and tune `z_fire/z_release` from real traffic.

## 7. Config (`DriftConfig`)

- Add `z_fire: f32` (default `2.5`), `z_release: f32` (default `1.0`).
- `threshold: f32` is **deprecated** — kept as a (now-ignored) field so existing agent TOMLs and the UI/DTO that still send it don't break serde load; a doc comment marks it dead. v2 never reads it.
- `baseline_turns` default `3 → 5`.
- Validation (`DriftConfig::validate`): `z_fire > 0`, `z_release > 0`, `z_release < z_fire`. Keep the existing `correct requires enabled` check. Drop/keep the old `threshold` range check as a no-op (it's dead but harmless).
- **UI follow-up (OUT OF SCOPE for this batch):** the Soul-tab drift section still shows `threshold`; a separate small UI task replaces it with `z_fire`/`z_release` inputs. Until then the operator sets them via TOML + restart (config-watcher doesn't watch agent TOMLs anyway).

## 8. Rollout

Backend-only, correction stays OFF on deploy. Sequence: ship v2 metric → keep `correct=false` on all agents → observe `drift_probe` z-scores in `session_timeline` for a real window → confirm the z distribution and that `z > z_fire` fires only on genuine outliers → then re-enable `correct=true` on Arty (canary) with validated `z_fire/z_release`. Mirrors the emotion/anchor canary pattern. No migration (payload is JSONB; config fields default).

## 9. Testing

**Pure (`drift/mod.rs` unit tests):**
- `baseline_stats`: centroid matches `centroid`; μ = mean and σ = popstd of distances-to-centroid on a hand-checked small set; `None` on empty/all-zero.
- `drift_zscore`: correct z on known μ/σ/dist; `SIGMA_FLOOR` applied when `σ < floor` (no divide-by-zero, bounded z).
- `hysteresis_decision`: below→above `z_fire` fires; above→below `z_release` releases; a z in `(z_release, z_fire)` HOLDS both prior states (active stays active, inactive stays inactive) — the defining Schmitt property.
- Edge: `baseline_turns=1` (σ=0 → floor); degenerate/zero embeddings → `None` path.

**Wiring:** the probe path is integration (needs embedder + cache); covered by review + the server run + the canary observation. No fabricated mock test.

## 10. Non-goals

- No stylometric / style-embedding signal (rejected in brainstorming: language-specific, noisy on short turns, over-engineered for a soft nudge) — accept the content-embedding residual, mitigated by self-calibration.
- No LLM-judge per-turn (too expensive on the hot context-build path).
- No change to the anchor block content, the injection point, `own_assistant_texts`, or the base+prompt-cache block ordering (pre-existing Minor#3).
- No UI change in this batch (§7 follow-up).
- No removal of the deprecated `threshold` field (wire-compat).
