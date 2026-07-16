# Persona-Drift v2 — Self-Calibrating z-score + Hysteresis — Design

**Date:** 2026-07-17
**Status:** Approved design (revised after tri-review), pre-implementation
**Area:** `crates/opex-core/src/agent/drift/mod.rs` (pure metric), `agent/engine/context_builder.rs` (probe wiring + per-session state), `config/mod.rs` (`DriftConfig` + validate + default tests), `gateway/handlers/agents/schema.rs` (`build_agent_config` DTO→config literal), `ui/src/app/(authenticated)/agents/` (new-agent default literal + inert-threshold input), `session_timeline` `drift_probe` payload.
**Source:** 2026-07-16 soul audit (opus). Revised after a 3-reviewer tri-review (statistics / state-concurrency / integration) that found the first draft's estimator carried a **systematic in-sample-centroid bias** that would have re-created v1's always-fire, plus concurrency and scope gaps.

## 1. Problem

v1: `drift = 1 − cos(recent_own_turn, centroid(first baseline_turns own-turns))`, flag when `drift > threshold` (absolute, default 0.15). In the 2560-dim toolgate embedding space the cosine distance between ANY two distinct turns sits ~0.3–0.8, so 0.15 demands near-paraphrase similarity → **36/36 prod probes fired**, making the injected identity anchor a permanent prompt block, not a correction. Deeper: full-turn embeddings encode **content (topic)** far more strongly than **style (persona)**, so the score is topic-dominated; the self-baseline only cancels topic within a topic-*stable* session. `correct=true` is currently disabled on Opex+Arty (detect-only retained for calibration).

v2 replaces the absolute constant with a **self-calibrating z-score** relative to the agent's own early-turn dispersion (subtracting the embedder floor + session topic-variance) plus **hysteresis** (so the anchor doesn't flap and thrash the prompt cache). The residual limitation — content-embeddings can't fully isolate persona from topic — is accepted: a genuinely novel topic *can* still trip the flag, but only as a statistical outlier vs the agent's own early spread. **The estimator must be bias-corrected (leave-one-out) or it silently reintroduces the v1 always-fire** (see §2, the single most important design constraint).

## 2. Metric (self-calibrating, bias-corrected z-score)

Baseline `B = [b_1 … b_n]` = the first `baseline_turns` own-assistant-turn embeddings that pass the length gate (§2.3). Recent own-turn embedding `r`.

### 2.1 Leave-one-out dispersion (bias correction — REQUIRED)

The naive estimator (μ/σ of `d_i = 1 − cos(b_i, centroid(B))`) is **systematically biased**: the centroid is computed *from* the baseline turns, so each `b_i` is artificially close to it, while the recent turn `r` is out-of-sample. At **zero drift**, `E[d_r] − μ_naive ≈ (1 − L²)/(n·L)` where `L = |centroid|` — for the observed regime (μ ≈ 0.3–0.5, n ≈ 5–8) that is **+0.15–0.30 in raw 1−cos ≈ +0.6–2.5 in z**, i.e. the same magnitude as v1's whole threshold. Naive z is therefore NOT zero-centered at no-drift; it re-creates always-fire.

Fix — **leave-one-out (LOO)** so every baseline sample is out-of-sample like `r`:

- `c = centroid(B)` (all `n`, full) — the reference used for the recent turn. Stored.
- For each `i`: `c₋ᵢ = centroid(B \ {b_i})`; `d_i = 1 − cos(b_i, c₋ᵢ)`.
- `μ = mean(d_i)`.
- `σ = sample_std(d_i)` — **Bessel-corrected (÷(n−1))**, NOT population std (popstd underestimates σ ~16% at small n, inflating z in the false-positive direction).

Each `d_i` now measures "one held-out turn vs a centroid of the others" — the same structure as `d_r` (`r` vs a centroid it isn't in), so the mean shift cancels. Residual: `c₋ᵢ` uses `n−1` turns vs `c`'s `n`, so `μ` is marginally over-estimated → z is slightly conservative → errs safe. (Pairwise dispersion is an equivalent bias-free alternative but requires storing all baseline embeddings to score `r` on the same scale — rejected for memory; LOO stores only `c + μ + σ`.)

### 2.2 z-score + effective σ floor

- Recent distance: `d_r = 1 − cos(r, c)`.
- `σ_eff = max(σ, SIGMA_FLOOR_ABS, SIGMA_FLOOR_REL · μ)` with `SIGMA_FLOOR_ABS = 0.05`, `SIGMA_FLOOR_REL = 0.2`. The absolute floor guards divide-by-≈0 on a near-identical baseline; the **relative** floor (`0.2·μ`) prevents a tight-but-narrow baseline from becoming hypersensitive — without it an innocent topic shift of `d_r − μ = 0.3` against `σ = 0.02` gives z = 15 (instant false fire).
- `z = (d_r − μ) / σ_eff`.
- Logged z is clamped to `[−Z_CAP, Z_CAP]`, `Z_CAP = 20` (keeps aggregate/percentile stats on the payload sane; does not change the fire decision).

### 2.3 Baseline quality gates

- **Length gate:** only own-turns with `content.trim().chars().count() ≥ MIN_BASELINE_CHARS (40)` count toward `baseline_turns`. Prevents 5 three-char confirmations ("да", "готово") from forming a meaningless centroid + near-zero σ. (`own_assistant_texts` already drops empty turns; this adds a min-length filter for baseline eligibility.)
- **Recent-turn exclusion:** the scored recent turn `r` MUST be strictly after the baseline window — never one of the `n` baseline turns (else `d_r` is in-sample → falsely low). The wiring guarantees this (probe only fires once ≥ `baseline_turns + 1` eligible own-turns exist, and `r` is the latest).
- `baseline_turns` default changes **3 → 8**: at n=5 the sample-std SE is ~±35% and the small-n reference is t(4) (`P(T>2.5) ≈ 3.3%` vs normal 0.6%); n=8 (t(7)) tightens both materially. Cost: the detector arms later (≥8 eligible own-turns ≈ ~16 total messages) — acceptable (short sessions don't drift).

### 2.4 Threshold framing

`z_fire`/`z_release` are **canary-calibrated starting points**, not derived from a normal-tail "0.6% outlier" claim — with n=8 the reference is ~t(7) and the distance distribution is right-skewed, so the true tail is heavier. The go/no-go calibration gate is empirical (§8): **median z on presumed-non-drift traffic must be ≈ 0**. Defaults `z_fire = 2.5`, `z_release = 1.0`.

## 3. Hysteresis (Schmitt trigger)

Per-session `anchor_active: bool`, two thresholds:

- `!active && z > z_fire`  → `active = true` (fire).
- `active && z < z_release` → `active = false` (release).
- otherwise (`z ∈ [z_release, z_fire]`) → hold. (Strict `>`/`<` with `z_release < z_fire` enforced by validate → the band uniformly holds either state, no gap/overlap.)

New sessions start `active = false`. The anchor is injected iff `active && correct`. While `active`, the anchor block is byte-identical every turn → prompt cache stable; flips occur only at genuine crossings. **This depends on the §2.1 bias fix:** without it the no-drift z floor (~+0.6–2.5) sits at/above `z_release=1.0`, so an `active` anchor would never release (v1's permanent block via another route). With LOO the no-drift floor returns to ≈0 and `z_release` is meaningful.

## 4. Per-session cache + state safety

Cache: `DashMap<Uuid /*session_id*/, CachedDrift>` on `AgentConfig` (existing `drift_baselines`). Entry: `CachedDrift { centroid: Vec<f32>, mu: f32, sigma: f32, anchor_active: bool }`. `centroid/mu/sigma` frozen at establishment; `anchor_active` mutated per probe. Memory: one vector + 3 scalars/session (baseline embeddings are NOT stored — LOO μ/σ computed once at establishment then discarded).

**State-safety invariants (concurrency — REQUIRED, not left to inference):**

1. **Establishment via `entry(session_id).or_insert_with(|| …)`, NOT a blind `insert()`.** v1's read-miss→compute→`insert` was safe because the value was just a centroid; in v2 a blind second insert would clobber a concurrently-updated `anchor_active` (lost fire). `or_insert_with` ensures a second concurrent establisher never overwrites live hysteresis state.
2. **The per-probe hysteresis RMW is a single `get_mut` critical section** spanning read + `hysteresis_decision` + write-back (all synchronous by that point — embeds already completed). Never `get()`-clone-then-later-`insert()`. Concurrent same-session probes ARE possible (a live channel/SSE probe and a cron `mirror_to_session` probe can hit the same active DM session), so the RMW must be atomic under the shard lock.
3. **Fail-soft holds state.** Any embed/degenerate failure (baseline establishment OR the recent-turn embed) returns before touching `anchor_active` → no anchor injected this turn, no state change. A transient embed hiccup must NOT release an active anchor (that would be a spurious flip/cache-miss). State-change happens only on a successful probe.

**Accepted debt (scoped to the `correct=false` canary window), documented not silently carried:**

4. **Eviction + rebuild degrades the baseline.** `history` fed to the probe is the already-truncated window (`max_history_messages`, 50). On first establishment (early session) "first `baseline_turns` own-turns" is genuinely the agent's early turns. But after eviction (soft-cap 2000, non-LRU `iter().next()` victim) + later rebuild, "first baseline_turns" is recomputed from the CURRENT window — a recent slice for a long session — which defeats the self-calibration premise AND resets `anchor_active` (possible anchor drop/flip). This is acceptable during the `correct=false` observation window (no prompt impact). **Before `correct=true` is enabled broadly, mitigate:** bias eviction away from `anchor_active == true` entries (cheap) and/or skip rebuild when the window no longer contains the true early turns. Tracked as a §8 gate, not shipped in this batch.
5. **`AgentConfig` rebuild resets the whole cache.** Any PUT-triggered agent-config reconstruction (icon, prompt, access — not just drift edits) rebuilds `drift_baselines` empty → every live session re-establishes baseline + resets hysteresis. Harmless with `correct=false`; revisit before broad `correct=true`.

## 5. Wiring (`context_builder.rs`)

Per probe (gated on `history.len() ≥ min_history` AND `≥ baseline_turns + 1` length-eligible own-turns):

1. `entry().or_insert_with`: if absent, embed the first `baseline_turns` eligible own-turns once via `cfg().embedder`, compute `(centroid, μ, σ)` via §2.1 LOO, freeze. Fail-soft → return, no state change.
2. Embed the recent own-turn → `d_r` → `z = drift_zscore(μ, σ, d_r)` (with §2.2 σ_eff). Fail-soft → return, no state change.
3. Single `get_mut`: `new_active = hysteresis_decision(z, entry.anchor_active, z_fire, z_release)`; `fired = new_active && !entry.anchor_active`; `entry.anchor_active = new_active`.
4. If `correct && new_active` → append `build_anchor_block(anchor, agent_name)` to the system-prompt tail (same injection point as v1).
5. Log `drift_probe` (§6) regardless of `correct`.

**Remove the dead v1 path:** delete the `over = score > cfg.threshold` variable and its threshold-keyed `warn!` log branch (they'd otherwise compile against the deprecated `threshold` field and emit a semantically-dead warning).

## 6. session_timeline `drift_probe` payload

Replace v1 `{score, corrected}` with `{ z, dist, mu, sigma, active, fired }` (`dist` = raw `d_r`; `z` clamped to Z_CAP; `active` = post-hysteresis; `fired` = crossed `!active→active` this turn). No consumer reads the old payload (grep-confirmed: only the writer; the Timeline UI renders generic JSON) → shape change is safe.

## 7. Config (`DriftConfig`) + call sites + UI

- Add `z_fire: f32` (`#[serde(default = "default_z_fire")]` = 2.5), `z_release: f32` (`default_z_release` = 1.0). The `#[serde(default)]` is REQUIRED so existing agent TOMLs lacking the fields load.
- `threshold: f32` is **deprecated** — kept (serde ignores nothing; the field simply stays) for wire-compat with the UI/DTO that still send `drift.threshold`; v2 never reads it. Doc-comment marks it dead. Its old range check in `validate` becomes a harmless no-op (keep or drop).
- `baseline_turns` default `3 → 8`.
- `validate`: add `z_fire > 0`, `z_release > 0`, `z_release < z_fire`. Existing configs (neither field → defaults 1.0 < 2.5) pass — no rejection risk.
- **Struct-literal call sites that MUST be updated (else compile break — these are IN SCOPE):**
  - `gateway/handlers/agents/schema.rs` `build_agent_config` — the exhaustive `DriftConfig { … }` literal (production DTO→config mapper): add `z_fire`/`z_release` from the payload (with the DTO carrying them, defaulting), and change the hardcoded `baseline_turns: d.baseline_turns.unwrap_or(3)` → `unwrap_or(8)` so it matches the new struct default (the two defaults must not diverge). The `DriftDto`/payload struct + generated TS types gain `z_fire`/`z_release` (optional).
  - `config/mod.rs` the `drift_config_validate_rejects_out_of_range` test literal — add the two fields.
- **Config default tests:** update `drift_config_defaults_when_section_absent` to assert `baseline_turns=8`, `z_fire=2.5`, `z_release=1.0` from an absent `[agent.drift]` section (this is the "5 live agents load fine" guarantee — make it a real test).
- **UI (in this batch — small):**
  - `ui/src/app/(authenticated)/agents/page.tsx` new-agent defaults: bump `driftBaselineTurns: "3"` → `"8"` (2-line; else UI-created agents bake the old default since `formToPayload` always sends the full drift object).
  - `AgentEditDialog.tsx` drift section: disable/gray the now-inert `threshold` input with a tooltip ("superseded by z_fire/z_release — config-only for now"). Prevents an operator from "raising the threshold" and seeing nothing happen during the canary. The full z_fire/z_release UI inputs are a **separate follow-up task** (out of scope here).

## 8. Rollout

Backend-first, `correct=false` on deploy (confirmed: with `correct=false` the new metric is computed + logged but the anchor is never injected → ZERO prompt/behavior change to live agents). Sequence:

1. Ship v2 metric, `correct=false` everywhere.
2. Observe `drift_probe` in `session_timeline` over real sessions. **Go/no-go gate: median z on presumed-non-drift traffic ≈ 0 and the z distribution roughly symmetric.** A materially-positive median z is the in-sample-bias signature (§2.1) — must be fixed, not thresholded around. (Note: with `baseline_turns=8` probes arm later per session — the observation window needs longer real sessions, not just calendar time, to accumulate a usable z sample.)
3. Only after the gate passes: address the §4.4 eviction mitigation, then re-enable `correct=true` on Arty (canary) with validated `z_fire/z_release`.

No migration (payload JSONB; config fields default).

## 9. Testing

**Pure (`drift/mod.rs`):**
- `baseline_stats` (LOO): on a hand-checked small set, μ = mean and σ = Bessel-std of the LOO distances; assert the LOO estimator is ~zero-biased on a synthetic no-drift set (all baseline turns iid around a mean + one held-out "recent" turn from the SAME distribution → z ≈ 0, NOT > 0 — this is the regression test for the C1 bias); `None` on empty/all-degenerate.
- `drift_zscore`: correct z on known μ/σ/dist; `σ_eff` applies the abs floor (σ<0.05→0.05) and the relative floor (0.2·μ) — assert a tight-narrow baseline doesn't yield z=15 on a modest shift; z clamped to ±Z_CAP.
- `hysteresis_decision`: below→above `z_fire` fires; above→below `z_release` releases; z in `(z_release, z_fire)` HOLDS both prior states (the Schmitt property); new-session inactive default.
- Baseline gates: min-length filter drops short turns; degenerate/zero embeddings → None.

**Config:** the updated default test (§7); the validate test with z fields.

**Wiring:** integration (needs embedder + cache) — covered by review + server run + canary observation; the RMW atomicity + fail-soft-holds-state invariants are code-review items, not unit-testable without a concurrency harness.

## 10. Non-goals

- No stylometric / style-embedding signal (rejected in brainstorming — language-specific, noisy on short turns) — accept the content-embedding residual, mitigated by LOO self-calibration.
- No LLM-judge per-turn (too expensive on the hot path).
- No pairwise-dispersion estimator (equivalent to LOO but needs stored baseline embeddings — rejected for memory).
- No change to the anchor content, injection point, or `own_assistant_texts` beyond the length gate.
- Full `z_fire/z_release` UI inputs — separate follow-up (this batch only bumps the baseline_turns default literal + disables the inert threshold input).
- §4.4 eviction mitigation (pin active sessions) — deferred to before broad `correct=true`, tracked as a §8 gate; accepted debt during the `correct=false` canary.
- No removal of the deprecated `threshold` field (wire-compat).
