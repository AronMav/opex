# Persona-Drift v2 — Batch D-R (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the broken absolute-threshold persona-drift metric with a bias-corrected (leave-one-out) self-calibrating z-score + Schmitt hysteresis, with concurrency-safe per-session state — so the identity anchor fires only on genuine outliers, not every turn.

**Architecture:** Pure metric in `drift/mod.rs` (LOO dispersion, z-score with a relative σ-floor, Schmitt decision) → config (`z_fire`/`z_release`, `baseline_turns` 3→8, deprecate `threshold`) → DTO mapper (`schema.rs`) → concurrency-safe wiring in `context_builder.rs` (`entry().or_insert_with` establishment + single-`get_mut` hysteresis RMW + fail-soft-holds-state). Ships with `correct=false` (zero prompt change); the anchor stays off until a canary calibration confirms median no-drift z ≈ 0.

**Tech Stack:** Rust 2024, dashmap, serde, sqlx. rustls-tls only.

## Global Constraints

- Rust + rustls-tls only — no new external dependency.
- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/`.
- Do NOT push, do NOT deploy — controller runs server tests + deploy after review, on explicit user approval.
- Windows dev host cannot run the bin-target / `#[sqlx::test]` suite — authority is the Linux server. Local gate: `cargo check -p opex-core --all-targets` + `cargo clippy -p opex-core --all-targets -- -D warnings`. (Pure `drift/mod.rs` tests DO run on Windows — they're DB-free — so run them there when possible.)
- **NO `Co-Authored-By` / Claude attribution trailer in ANY commit** — user forbids it. Subject line only.
- **The LOO bias correction (§2.1 of the spec) is non-negotiable** — a naive in-sample centroid re-creates v1's always-fire.
- Exact consts: `SIGMA_FLOOR_ABS=0.05`, `SIGMA_FLOOR_REL=0.2`, `Z_CAP=20.0`, `MIN_BASELINE_CHARS=40`, `baseline_turns` default `8`, `z_fire` default `2.5`, `z_release` default `1.0`.
- Preserve behaviour outside the drift metric; `correct=false` must mean ZERO prompt change.
- Source spec: `docs/superpowers/specs/2026-07-17-persona-drift-v2-zscore-design.md`.

## File Structure

- `crates/opex-core/src/agent/drift/mod.rs` — pure metric + `CachedDrift` (Task 1).
- `crates/opex-core/src/config/mod.rs` — `DriftConfig` fields/defaults/validate + tests (Task 2).
- `crates/opex-core/src/gateway/handlers/agents/schema.rs` — `DriftPayload` + `build_agent_config` literal (Task 3); regenerated TS types.
- `crates/opex-core/src/agent/engine/context_builder.rs` + `agent/agent_config.rs` — cache type + `drift_probe` rewrite (Task 4).

---

### Task 1: Pure metric — LOO dispersion, z-score, hysteresis, `CachedDrift`

**Files:**
- Modify: `crates/opex-core/src/agent/drift/mod.rs`

**Interfaces produced (consumed by Task 4):**
- `pub struct CachedDrift { pub centroid: Vec<f32>, pub mu: f32, pub sigma: f32, pub anchor_active: bool }` (`#[derive(Clone)]`)
- `pub fn baseline_stats(embeddings: &[Vec<f32>]) -> Option<(Vec<f32>, f32, f32)>`
- `pub fn drift_zscore(mu: f32, sigma: f32, recent_dist: f32) -> f32`
- `pub fn hysteresis_decision(z: f32, active: bool, z_fire: f32, z_release: f32) -> bool`
- consts `SIGMA_FLOOR_ABS`, `SIGMA_FLOOR_REL`, `Z_CAP`, `MIN_BASELINE_CHARS`
- REMOVED: `correction_anchor` + its test.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `drift/mod.rs`:

```rust
    #[test]
    fn baseline_stats_loo_is_unbiased_on_no_drift() {
        // 8 baseline turns iid around a direction + one "recent" from the SAME
        // distribution. With LOO, the recent z must be ≈ 0 (not systematically
        // positive) — this is the regression test for the in-sample bias.
        let dim = 64usize;
        let mk = |seed: u64| -> Vec<f32> {
            // deterministic pseudo-random unit-ish vector around e0 with noise
            let mut v = vec![0.0f32; dim];
            v[0] = 1.0;
            let mut s = seed.wrapping_mul(2654435761);
            for x in v.iter_mut() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                *x += ((s >> 33) as f32 / u32::MAX as f32 - 0.5) * 0.6;
            }
            v
        };
        let base: Vec<Vec<f32>> = (0..8).map(mk).collect();
        let (_c, mu, sigma) = baseline_stats(&base).expect("stats");
        assert!(sigma > 0.0, "sigma must be positive on varied baseline");
        // A held-out turn from the same distribution:
        let recent = mk(999);
        let (c, _, _) = baseline_stats(&base).unwrap();
        let d_r = drift_score(&c, &recent);
        let z = drift_zscore(mu, sigma, d_r);
        // Unbiased: |z| should be small (well under z_fire=2.5), NOT ~+1..2.5.
        assert!(z.abs() < 2.0, "no-drift z should be near 0, got {z} (bias?)");
    }

    #[test]
    fn drift_zscore_relative_floor_tames_narrow_baseline() {
        // tiny sigma but non-trivial mu → relative floor 0.2*mu dominates,
        // so a modest shift does NOT explode z.
        let mu = 0.30;
        let sigma = 0.001; // near-degenerate
        let d_r = mu + 0.30; // a "modest" shift equal to mu
        let z = drift_zscore(mu, sigma, d_r);
        // sigma_eff = max(0.001, 0.05, 0.2*0.30=0.06) = 0.06 → z = 0.30/0.06 = 5, clamped ok, but NOT 300.
        assert!(z <= Z_CAP && z < 6.0, "relative floor must cap sensitivity, got {z}");
    }

    #[test]
    fn drift_zscore_clamps_to_zcap() {
        let z = drift_zscore(0.1, 0.0001, 5.0); // huge
        assert!((z - Z_CAP).abs() < 1e-3, "z clamps to Z_CAP, got {z}");
    }

    #[test]
    fn hysteresis_schmitt_fire_release_hold() {
        // inactive: only fires above z_fire
        assert!(!hysteresis_decision(2.4, false, 2.5, 1.0));
        assert!(hysteresis_decision(2.6, false, 2.5, 1.0));
        // active: only releases below z_release
        assert!(hysteresis_decision(1.1, true, 2.5, 1.0));   // hold (in band)
        assert!(!hysteresis_decision(0.9, true, 2.5, 1.0));  // release
        // band holds BOTH states
        assert!(hysteresis_decision(1.5, true, 2.5, 1.0));   // stays active
        assert!(!hysteresis_decision(1.5, false, 2.5, 1.0)); // stays inactive
    }

    #[test]
    fn baseline_stats_none_below_two() {
        assert!(baseline_stats(&[]).is_none());
        assert!(baseline_stats(&[vec![1.0, 0.0]]).is_none()); // n<2 → no dispersion
    }
```

Also DELETE the existing `correction_anchor_gates_on_correct_and_threshold` test (it tests a fn being removed).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p opex-core --bin opex-core drift:: -- --nocapture` (runs on Windows — pure)
Expected: FAIL — `baseline_stats`/`drift_zscore`/`hysteresis_decision`/consts/`CachedDrift` undefined; `correction_anchor` test still references the (soon-removed) fn.

- [ ] **Step 3: Add consts + `CachedDrift`**

Near the top of `drift/mod.rs` (after the module doc):

```rust
/// Absolute floor on σ (divide-by-≈0 guard for a near-identical baseline).
pub const SIGMA_FLOOR_ABS: f32 = 0.05;
/// Relative floor on σ (× μ) — stops a tight-but-narrow baseline from becoming
/// hypersensitive to ordinary topic movement (spec §2.2).
pub const SIGMA_FLOOR_REL: f32 = 0.2;
/// Logged z is clamped to ±Z_CAP (keeps aggregate stats sane; not the fire gate).
pub const Z_CAP: f32 = 20.0;
/// Min chars for an own-turn to count toward the baseline (drops "да"/"готово").
pub const MIN_BASELINE_CHARS: usize = 40;

/// Per-session drift cache entry. `centroid/mu/sigma` are frozen at baseline
/// establishment; `anchor_active` is the mutable Schmitt-hysteresis state.
#[derive(Clone)]
pub struct CachedDrift {
    pub centroid: Vec<f32>,
    pub mu: f32,
    pub sigma: f32,
    pub anchor_active: bool,
}
```

- [ ] **Step 4: Add `baseline_stats` (LOO), `drift_zscore`, `hysteresis_decision`; remove `correction_anchor`**

Add (reusing the private `cosine` + existing `centroid`/`drift_score`):

```rust
/// Leave-one-out baseline stats → (full_centroid, μ, σ). Each baseline turn's
/// distance uses the centroid of the OTHER turns (out-of-sample, like the recent
/// turn), cancelling the in-sample-centroid bias that would otherwise re-create
/// v1's always-fire (spec §2.1). σ is the Bessel-corrected sample std (÷(n−1)).
/// `None` if fewer than 2 usable embeddings or all degenerate.
pub fn baseline_stats(embeddings: &[Vec<f32>]) -> Option<(Vec<f32>, f32, f32)> {
    let n = embeddings.len();
    if n < 2 {
        return None;
    }
    let full = centroid(embeddings)?;
    let mut dists = Vec::with_capacity(n);
    for i in 0..n {
        let others: Vec<Vec<f32>> = embeddings
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, e)| e.clone())
            .collect();
        let c_loo = centroid(&others)?;
        dists.push(1.0 - cosine(&embeddings[i], &c_loo));
    }
    let nf = n as f32;
    let mu = dists.iter().sum::<f32>() / nf;
    let var = dists.iter().map(|d| (d - mu).powi(2)).sum::<f32>() / (nf - 1.0);
    let sigma = var.sqrt();
    Some((full, mu, sigma))
}

/// z = (recent_dist − μ) / σ_eff, where σ_eff floors σ both absolutely and
/// relative to μ (spec §2.2). Result clamped to ±Z_CAP.
pub fn drift_zscore(mu: f32, sigma: f32, recent_dist: f32) -> f32 {
    let sigma_eff = sigma.max(SIGMA_FLOOR_ABS).max(SIGMA_FLOOR_REL * mu);
    let z = (recent_dist - mu) / sigma_eff;
    z.clamp(-Z_CAP, Z_CAP)
}

/// Schmitt trigger: fire above `z_fire`, release below `z_release`, else hold
/// the current state (spec §3). Returns the new `active`.
pub fn hysteresis_decision(z: f32, active: bool, z_fire: f32, z_release: f32) -> bool {
    if !active && z > z_fire {
        true
    } else if active && z < z_release {
        false
    } else {
        active
    }
}
```

DELETE `correction_anchor` (the whole fn, ~lines 65-80) — its absolute-threshold gating is superseded by `hysteresis_decision` in the wiring. Keep `build_anchor_block`, `centroid`, `cosine`, `drift_score`, `own_assistant_texts`.

- [ ] **Step 5: Run tests + gate**

Run: `cargo test -p opex-core --bin opex-core drift:: -- --nocapture` → PASS (6 new/updated).
`cargo check -p opex-core --all-targets` — expect errors ONLY in `context_builder.rs` (still calls the removed `correction_anchor` + old cache type — Task 4 fixes). Confirm the errors are confined there; `drift/mod.rs` itself compiles clean. `cargo clippy` deferred to Task 4 (crate-level).

- [ ] **Step 6: Commit** (NO trailer)

```bash
git add crates/opex-core/src/agent/drift/mod.rs
git commit -m "feat(drift): LOO-unbiased baseline_stats + z-score + Schmitt hysteresis + CachedDrift (v2 metric) [wiring in follow-up]"
```

---

### Task 2: Config — `z_fire`/`z_release`, `baseline_turns` 3→8, deprecate `threshold`

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (`DriftConfig` ~1480, its defaults, `Default`, `validate`, and the default/validate tests ~3634/3652)

**Interfaces produced:** `DriftConfig` gains `z_fire: f32`, `z_release: f32`; `baseline_turns` default is now 8.

- [ ] **Step 1: Update the default/validate tests (failing first)**

Find `drift_config_defaults_when_section_absent` (~3634) and change its assertions to the new defaults + add the new fields:

```rust
        assert_eq!(cfg.agent.drift.baseline_turns, 8);
        assert!((cfg.agent.drift.z_fire - 2.5).abs() < f32::EPSILON);
        assert!((cfg.agent.drift.z_release - 1.0).abs() < f32::EPSILON);
```

Find `drift_config_validate_rejects_out_of_range` (~3652) — the exhaustive `DriftConfig { … }` literal — and add `z_fire: 2.5, z_release: 1.0,` to it (else it won't compile). Add one assertion that `z_release >= z_fire` is rejected:

```rust
        let bad_z = DriftConfig { z_fire: 1.0, z_release: 2.0, ..DriftConfig::default() };
        assert!(bad_z.validate().iter().any(|e| e.contains("z_release")), "z_release>=z_fire must be rejected");
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p opex-core --bin opex-core drift_config -- --nocapture` (Windows OK — pure config)
Expected: FAIL to compile (fields/defaults missing).

- [ ] **Step 3: Add the fields + defaults**

In `DriftConfig` (after `baseline_turns`, before `correct`):

```rust
    #[serde(default = "default_drift_z_fire")]
    pub z_fire: f32,
    #[serde(default = "default_drift_z_release")]
    pub z_release: f32,
```

Mark `threshold` deprecated (doc comment above it — keep the field for UI/DTO wire-compat, v2 never reads it):

```rust
    /// DEPRECATED (v2): the absolute-threshold metric is replaced by the
    /// z-score (`z_fire`/`z_release`). Kept only for wire-compat with agent
    /// TOMLs / the UI DTO that still send `drift.threshold`; never read.
    #[serde(default = "default_drift_threshold")]
    pub threshold: f32,
```

Change `default_drift_baseline_turns` to return `8`. Add:

```rust
fn default_drift_z_fire() -> f32 { 2.5 }
fn default_drift_z_release() -> f32 { 1.0 }
```

Add the two fields to the `Default for DriftConfig` impl (`z_fire: default_drift_z_fire(), z_release: default_drift_z_release()`).

- [ ] **Step 4: Validate**

In `DriftConfig::validate`, add (keep the existing `correct requires enabled`; the old `threshold` range check may stay as a harmless no-op or be dropped):

```rust
        if self.z_fire <= 0.0 {
            errors.push("drift.z_fire must be > 0".to_string());
        }
        if self.z_release <= 0.0 {
            errors.push("drift.z_release must be > 0".to_string());
        }
        if self.z_release >= self.z_fire {
            errors.push("drift.z_release must be < drift.z_fire".to_string());
        }
```

- [ ] **Step 5: Run tests + gate**

Run: `cargo test -p opex-core --bin opex-core drift_config -- --nocapture` → PASS.
`cargo check -p opex-core --all-targets` — remaining errors only in schema.rs (Task 3 literal) + context_builder (Task 4).

- [ ] **Step 6: Commit** (NO trailer)

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(drift): DriftConfig z_fire/z_release, baseline_turns default 8, deprecate threshold"
```

---

### Task 3: DTO mapper — `DriftPayload` + `build_agent_config`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/schema.rs` (`DriftPayload` ~200, `build_agent_config` drift literal ~277-284)
- Regenerate: TS types (the gen-types step)

**Background:** `build_agent_config` maps the create/PUT payload to `DriftConfig` via an EXHAUSTIVE struct literal (~277-284) — adding `z_fire`/`z_release` to `DriftConfig` (Task 2) breaks it until this literal is updated. It also hardcodes `baseline_turns: d.baseline_turns.unwrap_or(3)`, which must become `unwrap_or(8)` to match the new struct default (the two defaults must not diverge).

- [ ] **Step 1: Add fields to `DriftPayload`**

In `DriftPayload` (~200), add:

```rust
    pub z_fire: Option<f32>,
    pub z_release: Option<f32>,
```

- [ ] **Step 2: Update the `build_agent_config` literal**

At ~277-284, in the `DriftConfig { … }` literal, change `baseline_turns` and add the two fields:

```rust
            drift: p.drift.flatten().map(|d| DriftConfig {
                enabled: d.enabled.unwrap_or(false),
                threshold: d.threshold.unwrap_or(0.15), // deprecated, ignored by v2
                min_history: d.min_history.unwrap_or(6),
                baseline_turns: d.baseline_turns.unwrap_or(8),
                z_fire: d.z_fire.unwrap_or(2.5),
                z_release: d.z_release.unwrap_or(1.0),
                correct: d.correct.unwrap_or(false),
                anchor: d.anchor,
            }),
```

(Match the actual field names/order of the current literal — the above shows the additions; keep every existing field. If the literal uses `..Default::default()` it would not have broken, but it is exhaustive per the review — add the two fields.)

- [ ] **Step 3: Build + regenerate TS types**

Run: `cargo check -p opex-core --all-targets` — schema.rs now compiles; remaining errors only in context_builder.rs (Task 4).
Regenerate the TS types the project uses (find the gen-types command — e.g. `cargo test -p opex-core export_bindings` or a `make gen-types` / `ts-rs` export; check CLAUDE.md / Makefile). The new optional `z_fire`/`z_release` appear in the generated `AgentInfo`/drift DTO TS type. (The UI does not yet SEND them — that's the deferred UI task; they default in `build_agent_config`.)

- [ ] **Step 4: Commit** (NO trailer)

```bash
git add crates/opex-core/src/gateway/handlers/agents/schema.rs ui/src/types/api.generated.ts
git commit -m "feat(drift): DriftPayload z_fire/z_release + build_agent_config (baseline_turns default 8)"
```

(Adjust the generated-types path to the real one.)

---

### Task 4: Wiring — cache type + concurrency-safe `drift_probe`

**Files:**
- Modify: `crates/opex-core/src/agent/agent_config.rs` (`drift_baselines` field type ~95)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` (`drift_probe` ~281-362)

**Interfaces consumed:** `CachedDrift`, `baseline_stats`, `drift_zscore`, `hysteresis_decision`, consts (Task 1); `cfg.z_fire`/`z_release`/`baseline_turns` (Task 2).

**Background:** the cache is `DashMap<Uuid, Arc<Vec<f32>>>` (Arc-centroid). v2 stores `CachedDrift` (owned, mutated via `get_mut`). The rewrite must honor the spec's concurrency invariants: establish via `entry().or_insert_with` (not blind insert), do the hysteresis read-modify-write under a single `get_mut`, and fail-soft must leave state untouched.

- [ ] **Step 1: Change the cache field type**

In `agent_config.rs` (~95):

```rust
    pub drift_baselines: std::sync::Arc<dashmap::DashMap<uuid::Uuid, crate::agent::drift::CachedDrift>>,
```

(Update the doc comment: entry now carries frozen `centroid/mu/sigma` + mutable `anchor_active`; note the AgentConfig-rebuild reset wipes hysteresis state for all sessions — accepted during the `correct=false` canary window, spec §4.5.)

- [ ] **Step 2: Rewrite `drift_probe`**

Replace the body of `drift_probe` (lines ~282-362) with:

```rust
        let cfg = &self.cfg().agent.drift;
        if !cfg.enabled {
            return None;
        }
        if history.len() < cfg.min_history {
            return None;
        }
        let agent = self.agent_name();
        // Length-gated own-turns eligible for the baseline (drops trivial turns).
        let all_texts = crate::agent::drift::own_assistant_texts(history, agent);
        let eligible: Vec<&String> = all_texts
            .iter()
            .filter(|t| t.chars().count() >= crate::agent::drift::MIN_BASELINE_CHARS)
            .collect();
        // Need ≥ baseline_turns eligible + ≥1 recent (the latest own-turn).
        if eligible.len() < cfg.baseline_turns + 1 {
            return None;
        }
        let embedder = &self.cfg().embedder;
        let baselines = &self.cfg().drift_baselines;

        // 1. Establish (once) via or_insert_with — never a blind insert that could
        //    clobber a concurrently-updated anchor_active (spec §4.1). We compute
        //    the CachedDrift OUTSIDE the map first (embed is async), then insert
        //    only if still absent.
        if !baselines.contains_key(&session_id) {
            let base_texts: Vec<&str> =
                eligible.iter().take(cfg.baseline_turns).map(|s| s.as_str()).collect();
            let embs = match embedder.embed_batch(&base_texts).await {
                Ok(e) => e,
                Err(e) => { tracing::warn!(agent, error = %e, "drift baseline embed failed"); return None; }
            };
            let Some((centroid, mu, sigma)) = crate::agent::drift::baseline_stats(&embs) else {
                tracing::warn!(agent, "drift baseline degenerate"); return None;
            };
            // soft-cap backstop (unchanged eviction policy; §4.4 accepted debt).
            const MAX_BASELINES: usize = 2000;
            if baselines.len() >= MAX_BASELINES {
                let victim = baselines.iter().next().map(|e| *e.key());
                if let Some(k) = victim { baselines.remove(&k); }
            }
            baselines
                .entry(session_id)
                .or_insert(crate::agent::drift::CachedDrift { centroid, mu, sigma, anchor_active: false });
        }

        // 2. Recent turn embed → z. Fail-soft: return without touching state.
        let recent_text = eligible.last()?;
        let recent = match embedder.embed(recent_text).await {
            Ok(v) => v,
            Err(e) => { tracing::warn!(agent, error = %e, "drift recent embed failed"); return None; }
        };
        // Read frozen μ/σ/centroid (clone the small parts) to compute z, then do
        // the hysteresis RMW under a SEPARATE single get_mut (steps 2+3 split so
        // no async await is held across the shard lock).
        let (mu, sigma, dist) = {
            let Some(entry) = baselines.get(&session_id) else { return None }; // evicted mid-flight
            let dist = crate::agent::drift::drift_score(&entry.centroid, &recent);
            (entry.mu, entry.sigma, dist)
        };
        let z = crate::agent::drift::drift_zscore(mu, sigma, dist);

        // 3. Hysteresis RMW under a single get_mut critical section (spec §4.2).
        let (active, fired) = {
            let Some(mut entry) = baselines.get_mut(&session_id) else { return None };
            let prev = entry.anchor_active;
            let new_active = crate::agent::drift::hysteresis_decision(z, prev, cfg.z_fire, cfg.z_release);
            entry.anchor_active = new_active;
            (new_active, new_active && !prev)
        };

        // 4. Inject iff correct && active.
        let anchor = if cfg.correct && active {
            Some(crate::agent::drift::build_anchor_block(cfg.anchor.as_deref(), agent))
        } else {
            None
        };

        // 5. Log (regardless of correct).
        let payload = serde_json::json!({
            "z": z,
            "dist": dist,
            "mu": mu,
            "sigma": sigma,
            "active": active,
            "fired": fired,
            "own_assistant_turns": all_texts.len(),
            "baseline_turns_used": cfg.baseline_turns,
            "history_len": history.len(),
        });
        if let Err(e) = opex_db::session_timeline::log_event(
            &self.cfg().db, session_id, "drift_probe", Some(&payload),
        ).await {
            tracing::warn!(agent, error = %e, "drift timeline write failed");
        }
        if anchor.is_some() {
            tracing::info!(agent, z, "drift anchor active");
        } else {
            tracing::debug!(agent, z, active, "drift probe");
        }
        anchor
```

Note: the old `over`/`threshold` variable and its `warn!` branch are gone (the deprecated `threshold` is never read). The two async embeds are awaited BEFORE any `get_mut`, so no lock is held across `.await` (avoids the DashMap-guard-across-await hazard).

- [ ] **Step 3: Whole-crate build + clippy**

Run: `cargo check -p opex-core --all-targets` → 0 errors (all references to the removed `correction_anchor` + old cache type are gone). `cargo clippy -p opex-core --all-targets -- -D warnings` → 0 warnings.

- [ ] **Step 4: Commit** (NO trailer)

```bash
git add crates/opex-core/src/agent/agent_config.rs crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "feat(drift): concurrency-safe drift_probe — LOO establish + get_mut hysteresis RMW + z payload (#v2)"
```

---

## Post-implementation (controller, after whole-branch review + user approval)

- Server test session (throttled): `cargo test -p opex-core --bin opex-core drift` + `drift_config` + `cargo clippy --all-targets -D warnings`. No migration.
- Deploy: throttled release build + `server-deploy.sh --skip-build` + restart. `correct=false` stays on all agents (zero prompt change).
- Post-deploy: confirm `drift_probe` payloads now carry `{z, dist, mu, sigma, active, fired}`; **calibration gate before any `correct=true`: median z on real (presumed non-drift) traffic ≈ 0** (a materially-positive median = residual bias, must be fixed not thresholded). Longer real sessions needed (baseline arms at ≥8 eligible own-turns).

## Depends on / pairs with

- UI batch (`2026-07-17-persona-drift-v2-batch-u-ui.md`): bumps the new-agent `baseline_turns` default literal 3→8 and greys the inert `threshold` input. Deploy via `deploy-ui.sh`. Independent; either order.
