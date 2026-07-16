# Soul-layer UI editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the per-agent soul layer (`[agent.soul]`, `[agent.drift]`, `[agent.initiative]`, `[agent.emotion]`) editable from the agent editor UI, with server-side validation and README documentation.

**Architecture:** Backend gains a reusable `validate_sections()` (called by both `load()` and the create/update handlers), four new GET DTO sub-structs, four payload sub-structs, and a presence-gated preserve-merge that lets UI edits win while still preserving disk config for omitted sections. UI adds a "Soul" tab whose form logic lives in `agents/page.tsx`. README gets one factual differentiator row.

**Tech Stack:** Rust (axum, serde, toml, ts-rs), Next.js/React/TypeScript, vitest.

**Spec:** `docs/superpowers/specs/2026-07-16-soul-layer-ui-editor-design.md` — the §Config surface table (field names, types, ranges, defaults) is the authoritative field reference for every task below.

## Global Constraints

- Rust: rustls-tls only, never add OpenSSL. Edition 2024.
- Commits: NO `Co-Authored-By` / Claude attribution (project rule). Work directly in `master`.
- TDD: failing test first, watch it fail, minimal code, watch it pass, commit.
- Rust tests for `opex-core` run in the **bin** target: `cargo test -p opex-core --bin opex-core <filter>`. DB-backed `#[sqlx::test]` tests fail with `EnvVar(NotPresent)` without `DATABASE_URL` — that is expected; ignore those two pre-existing failures.
- `clippy -D warnings` is NOT caught by `cargo check` — run `cargo clippy -p opex-core --bin opex-core` before finishing a Rust task.
- `ui/src/types/api.generated.ts` is generated — NEVER hand-edit it. Regenerate with `make gen-types` (`cargo run --features ts-gen --bin gen_ts_types -p opex-core`). `make` is not on PATH on Windows; run the raw `cargo run` command if `make` is unavailable.
- vitest runs ONLY from `ui/`: `cd ui && npx vitest run <file>`.
- Field names / ranges / defaults come verbatim from the spec §Config surface table. Do not invent values.

---

### Task 1: Extract `validate_sections()` in config/mod.rs

Behavior-preserving refactor: pull the per-section `validate()` calls + the four cross-field `bail!` checks out of `AgentConfig::load()` into a reusable method that returns error strings instead of bailing, so the create/update handlers can call it too.

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (`load()` body at ~2121-2205; add `validate_sections` method on `AgentConfig`)
- Test: same file, `#[cfg(test)] mod validate_sections_tests`

**Interfaces:**
- Produces: `impl AgentConfig { pub fn validate_sections(&self) -> Vec<String> }` — returns all section + cross-field error messages (empty = valid). Consumed by Task 2 and by `load()`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod validate_sections_tests {
    use super::*;

    fn base_cfg() -> AgentConfig {
        toml::from_str("[agent]\nname = \"T\"\nprovider = \"openai\"\nmodel = \"gpt-4o\"\n").unwrap()
    }

    #[test]
    fn emotion_without_soul_is_rejected() {
        let mut c = base_cfg();
        c.agent.emotion.enabled = true; // soul stays disabled
        let errs = c.validate_sections();
        assert!(errs.iter().any(|e| e.contains("[agent.emotion]") && e.contains("[agent.soul]")),
            "expected emotion-requires-soul error, got: {errs:?}");
    }

    #[test]
    fn valid_config_has_no_errors() {
        assert!(base_cfg().validate_sections().is_empty());
    }

    #[test]
    fn daily_plan_without_heartbeat_is_rejected() {
        let mut c = base_cfg();
        c.agent.initiative.enabled = true;
        c.agent.initiative.daily_plan = true; // no heartbeat
        let errs = c.validate_sections();
        assert!(errs.iter().any(|e| e.contains("daily_plan") && e.contains("heartbeat")),
            "expected daily_plan-requires-heartbeat error, got: {errs:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core validate_sections_tests`
Expected: FAIL — `no method named validate_sections`.

- [ ] **Step 3: Add the method and refactor `load()` to use it**

Add this method to `impl AgentConfig` (near `load`):

```rust
/// Collect all section + cross-field validation errors (empty = valid).
/// Single source of truth shared by `load()` (startup/reload) and the
/// create/update HTTP handlers (pre-write). Mirrors the checks that were
/// previously inlined in `load()`.
pub fn validate_sections(&self) -> Vec<String> {
    let mut errs = Vec::new();
    let tag = |section: &str, list: Vec<String>| -> Vec<String> {
        list.into_iter().map(|e| format!("[agent.{section}] {e}")).collect()
    };
    errs.extend(tag("delegation", self.agent.delegation.validate()));
    errs.extend(tag("soul", self.agent.soul.validate()));
    errs.extend(tag("drift", self.agent.drift.validate()));
    errs.extend(tag("initiative", self.agent.initiative.validate()));
    errs.extend(tag("emotion", self.agent.emotion.validate()));
    if self.agent.initiative.daily_plan && self.agent.heartbeat.is_none() {
        errs.push("[agent.initiative] daily_plan=true requires a configured [agent.heartbeat]".into());
    }
    if self.agent.initiative.daily_plan && !self.agent.initiative.enabled {
        errs.push("[agent.initiative] daily_plan=true requires enabled=true".into());
    }
    if self.agent.emotion.enabled && !self.agent.soul.enabled {
        errs.push("[agent.emotion] enabled=true requires [agent.soul] enabled=true".into());
    }
    errs
}
```

Then replace the inline validation block in `load()` (the five `validate()`/`bail!` groups + three cross-field `bail!`s at ~2129-2203) with:

```rust
let section_errors = config.validate_sections();
if !section_errors.is_empty() {
    anyhow::bail!(
        "agent {:?}: invalid config:\n  - {}",
        config.agent.name,
        section_errors.join("\n  - ")
    );
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p opex-core --bin opex-core validate_sections_tests`
Expected: PASS (3 tests). Also run the existing config tests to confirm no regression: `cargo test -p opex-core --bin opex-core config::` — expected PASS (DB-gated tests excepted).

- [ ] **Step 5: clippy + commit**

Run: `cargo clippy -p opex-core --bin opex-core` (expect clean).

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "refactor(config): extract AgentConfig::validate_sections() from load()"
```

---

### Task 2: Validate config sections in create/update handlers before write

Close the gap where the handlers write TOML without running section validation. Call `validate_sections()` right before `cfg.to_toml()` in both the create and update paths; return `400` with the joined messages.

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/crud.rs` (create path before `cfg.to_toml()` at ~490; update path before `cfg.to_toml()` at ~787)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `AgentConfig::validate_sections()` (Task 1).

- [ ] **Step 1: Write the failing test**

Add to `crud.rs` `mod tests`:

```rust
#[test]
fn validate_sections_rejects_emotion_without_soul() {
    // Exercises the shared validator the handlers now call pre-write.
    let mut cfg: AgentConfig = toml::from_str(
        "[agent]\nname = \"T\"\nprovider = \"openai\"\nmodel = \"gpt-4o\"\n\
         [agent.emotion]\nenabled = true\n",
    ).unwrap();
    cfg.agent.soul.enabled = false;
    let errs = cfg.validate_sections();
    assert!(!errs.is_empty(), "emotion without soul must be invalid");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core validate_sections_rejects_emotion_without_soul`
Expected: FAIL to compile until `AgentConfig` is imported in the test module (if not already). If Task 1 is merged, it may already pass at the unit level — the real deliverable is the handler wiring in Step 3, which has no cheap unit test (the handler is a large async fn touching disk/DB). Treat Step 1 as the guard that `validate_sections` is reachable; verify the handler wiring by the manual check in Step 4.

- [ ] **Step 3: Wire validation into both handler paths**

In the **create** handler, immediately before `let toml_str = match cfg.to_toml()` (~490):

```rust
let section_errors = cfg.validate_sections();
if !section_errors.is_empty() {
    return (StatusCode::BAD_REQUEST, Json(json!({"error": section_errors.join("; ")}))).into_response();
}
```

In the **update** handler, immediately before `let toml_str = match cfg.to_toml()` (~787), add the identical block. (Place it AFTER the preserve/merge logic so merged values are validated.)

- [ ] **Step 4: Verify**

Run: `cargo test -p opex-core --bin opex-core validate_sections_rejects_emotion_without_soul` → PASS.
Run: `cargo clippy -p opex-core --bin opex-core` → clean.
Manual grep check: `grep -n "validate_sections" crates/opex-core/src/gateway/handlers/agents/crud.rs` → expect two call sites (create + update).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/agents/crud.rs
git commit -m "fix(agents): validate config sections in create/update before write (400 on invalid)"
```

---

### Task 3: GET DTO — expose soul/drift/initiative/emotion

Add four nested DTO structs and populate them so `GET /api/agents/{name}` returns the four sections. Regenerate TS types.

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/dto_structs.rs` (add 4 structs + 4 fields on `AgentDetailDto`)
- Modify: `crates/opex-core/src/gateway/handlers/agents/dto.rs` (populate the 4 fields)
- Regen: `ui/src/types/api.generated.ts` (via `make gen-types`)
- Test: `crates/opex-core/src/gateway/handlers/agents/dto.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `AgentDetailDto.soul: AgentDetailSoulDto`, `.drift: AgentDetailDriftDto`, `.initiative: AgentDetailInitiativeDto`, `.emotion: AgentDetailEmotionDto` (non-`Option`, always present). Consumed by Task 6 (`detailToForm`).

- [ ] **Step 1: Add the DTO structs**

In `dto_structs.rs`, after `AgentDetailToolLoopDto`, add (following that struct's derive + `register_ts_dto!` pattern; note the `ts(type = "number")` annotation on the u64 field, mirroring `daily_budget_tokens`):

```rust
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailSoulDto {
    pub enabled: bool,
    pub reflection_threshold: f64,
    pub reflection_cooldown_minutes: u64,
    pub context_top_k: usize,
    pub context_budget_tokens: u32,
    pub max_events_per_session: usize,
}
crate::register_ts_dto!(AgentDetailSoulDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailDriftDto {
    pub enabled: bool,
    pub threshold: f32,
    pub min_history: usize,
    pub baseline_turns: usize,
    pub correct: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-gen", ts(optional))]
    pub anchor: Option<String>,
}
crate::register_ts_dto!(AgentDetailDriftDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailInitiativeDto {
    pub enabled: bool,
    pub daily_proposal_cap: u32,
    pub decompose: bool,
    pub daily_plan: bool,
    pub auto_approve_day_plan: bool,
    #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
    pub daily_token_budget: u64,
}
crate::register_ts_dto!(AgentDetailInitiativeDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct AgentDetailEmotionDto {
    pub enabled: bool,
    pub intensity_importance_k: f32,
    pub blend_rate: f32,
    pub decay_half_life_hours: f32,
}
crate::register_ts_dto!(AgentDetailEmotionDto);
```

Then add four fields to `AgentDetailDto` (near `tool_dispatcher`):

```rust
    pub soul: AgentDetailSoulDto,
    pub drift: AgentDetailDriftDto,
    pub initiative: AgentDetailInitiativeDto,
    pub emotion: AgentDetailEmotionDto,
```

- [ ] **Step 2: Populate them in `dto.rs`**

In the `AgentDetailDto { ... }` constructor, add (these `AgentSettings` fields are non-`Option`, so no `.as_ref().map`):

```rust
            soul: AgentDetailSoulDto {
                enabled: a.soul.enabled,
                reflection_threshold: a.soul.reflection_threshold,
                reflection_cooldown_minutes: a.soul.reflection_cooldown_minutes,
                context_top_k: a.soul.context_top_k,
                context_budget_tokens: a.soul.context_budget_tokens,
                max_events_per_session: a.soul.max_events_per_session,
            },
            drift: AgentDetailDriftDto {
                enabled: a.drift.enabled,
                threshold: a.drift.threshold,
                min_history: a.drift.min_history,
                baseline_turns: a.drift.baseline_turns,
                correct: a.drift.correct,
                anchor: a.drift.anchor.clone(),
            },
            initiative: AgentDetailInitiativeDto {
                enabled: a.initiative.enabled,
                daily_proposal_cap: a.initiative.daily_proposal_cap,
                decompose: a.initiative.decompose,
                daily_plan: a.initiative.daily_plan,
                auto_approve_day_plan: a.initiative.auto_approve_day_plan,
                daily_token_budget: a.initiative.daily_token_budget,
            },
            emotion: AgentDetailEmotionDto {
                enabled: a.emotion.enabled,
                intensity_importance_k: a.emotion.intensity_importance_k,
                blend_rate: a.emotion.blend_rate,
                decay_half_life_hours: a.emotion.decay_half_life_hours,
            },
```

Import the new struct names at the top of `dto.rs` if the module uses explicit imports (check existing `use super::dto_structs::...`).

- [ ] **Step 3: Write a test asserting the DTO carries the values**

In `dto.rs` `#[cfg(test)]` (or add one), build an `AgentConfig` with soul enabled and assert the DTO reflects it. If `dto.rs` has no test harness that constructs `AgentDetailDto` cheaply (it needs `AppState`-ish inputs), instead add the assertion at the struct level:

```rust
#[test]
fn soul_dto_fields_present() {
    // Compile-time guard: the four soul fields exist and are non-Option.
    fn _assert(d: &super::dto_structs::AgentDetailDto) {
        let _ = (d.soul.enabled, d.drift.enabled, d.initiative.enabled, d.emotion.enabled);
    }
}
```

- [ ] **Step 4: Build + regenerate types**

Run: `cargo test -p opex-core --bin opex-core soul_dto_fields_present` → PASS.
Regenerate: `cargo run --features ts-gen --bin gen_ts_types -p opex-core` (or `make gen-types`).
Verify: `grep -n "AgentDetailSoulDto" ui/src/types/api.generated.ts` → expect the generated interface and a `soul:` field on `AgentDetailDto`.

- [ ] **Step 5: clippy + commit**

Run: `cargo clippy -p opex-core --bin opex-core` → clean.

```bash
git add crates/opex-core/src/gateway/handlers/agents/dto_structs.rs crates/opex-core/src/gateway/handlers/agents/dto.rs ui/src/types/api.generated.ts
git commit -m "feat(agents): expose soul/drift/initiative/emotion in agent detail DTO"
```

---

### Task 4: Payload sub-structs + build_agent_config mapping

Let the create/update payload carry the four sections and map them into the config (replacing the hardcoded `::default()`).

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/schema.rs` (`AgentCreatePayload` + payload sub-structs + `build_agent_config` + `emptyForm`-equivalent `None` defaults in the test-support `Default`)
- Test: `schema.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `AgentCreatePayload.soul: Option<Option<SoulPayload>>` (+ drift/initiative/emotion). Consumed by Task 5 (presence detection) and build mapping.

- [ ] **Step 1: Write the failing test**

`AgentCreatePayload` does NOT derive `Default`; tests construct it via the
existing `minimal_payload(name)` helper (schema.rs `mod tests`, ~356). Use it:

```rust
#[test]
fn build_agent_config_maps_soul_payload() {
    let mut p = minimal_payload("T");
    p.soul = Some(Some(SoulPayload {
        enabled: Some(true),
        reflection_threshold: Some(200.0),
        ..Default::default() // SoulPayload DOES derive Default (Step 3)
    }));
    let cfg = build_agent_config("T".into(), p);
    assert!(cfg.agent.soul.enabled);
    assert_eq!(cfg.agent.soul.reflection_threshold, 200.0);
    // unset fields fall back to config defaults
    assert_eq!(cfg.agent.soul.context_top_k, 6);
}

#[test]
fn build_agent_config_absent_soul_is_default() {
    let cfg = build_agent_config("T".into(), minimal_payload("T"));
    assert!(!cfg.agent.soul.enabled);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core build_agent_config_maps_soul_payload`
Expected: FAIL — `SoulPayload` unknown / field missing on `AgentCreatePayload`.

- [ ] **Step 3: Add payload structs + fields + mapping**

Add payload sub-structs (mirror the existing `ToolLoopPayload` shape — all fields `Option<T>`, `#[derive(Debug, Default, Deserialize)]`; use `serde default` so partial payloads deserialize):

```rust
#[derive(Debug, Default, Deserialize)]
pub struct SoulPayload {
    pub enabled: Option<bool>,
    pub reflection_threshold: Option<f64>,
    pub reflection_cooldown_minutes: Option<u64>,
    pub context_top_k: Option<usize>,
    pub context_budget_tokens: Option<u32>,
    pub max_events_per_session: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DriftPayload {
    pub enabled: Option<bool>,
    pub threshold: Option<f32>,
    pub min_history: Option<usize>,
    pub baseline_turns: Option<usize>,
    pub correct: Option<bool>,
    pub anchor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct InitiativePayload {
    pub enabled: Option<bool>,
    pub daily_proposal_cap: Option<u32>,
    pub decompose: Option<bool>,
    pub daily_plan: Option<bool>,
    pub auto_approve_day_plan: Option<bool>,
    pub daily_token_budget: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct EmotionPayload {
    pub enabled: Option<bool>,
    pub intensity_importance_k: Option<f32>,
    pub blend_rate: Option<f32>,
    pub decay_half_life_hours: Option<f32>,
}
```

Add fields to `AgentCreatePayload` (mirroring `tool_loop: Option<Option<ToolLoopPayload>>`):

```rust
    #[serde(default)]
    pub soul: Option<Option<SoulPayload>>,
    #[serde(default)]
    pub drift: Option<Option<DriftPayload>>,
    #[serde(default)]
    pub initiative: Option<Option<InitiativePayload>>,
    #[serde(default)]
    pub emotion: Option<Option<EmotionPayload>>,
```

In `build_agent_config`, replace the four hardcoded lines (`soul: SoulConfig::default(),` etc.) with defaults-preserving maps. Fields absent in the payload fall back to the config `default()` values from the ranges table:

```rust
            soul: p.soul.flatten().map(|s| SoulConfig {
                enabled: s.enabled.unwrap_or(false),
                reflection_threshold: s.reflection_threshold.unwrap_or(150.0),
                reflection_cooldown_minutes: s.reflection_cooldown_minutes.unwrap_or(60),
                context_top_k: s.context_top_k.unwrap_or(6),
                context_budget_tokens: s.context_budget_tokens.unwrap_or(800),
                max_events_per_session: s.max_events_per_session.unwrap_or(10),
            }).unwrap_or_default(),
            drift: p.drift.flatten().map(|d| DriftConfig {
                enabled: d.enabled.unwrap_or(false),
                threshold: d.threshold.unwrap_or(0.15),
                min_history: d.min_history.unwrap_or(6),
                baseline_turns: d.baseline_turns.unwrap_or(3),
                correct: d.correct.unwrap_or(false),
                anchor: d.anchor.filter(|s| !s.is_empty()),
            }).unwrap_or_default(),
            initiative: p.initiative.flatten().map(|i| InitiativeConfig {
                enabled: i.enabled.unwrap_or(false),
                daily_proposal_cap: i.daily_proposal_cap.unwrap_or(1),
                decompose: i.decompose.unwrap_or(false),
                daily_plan: i.daily_plan.unwrap_or(false),
                auto_approve_day_plan: i.auto_approve_day_plan.unwrap_or(false),
                daily_token_budget: i.daily_token_budget.unwrap_or(0),
            }).unwrap_or_default(),
            emotion: p.emotion.flatten().map(|e| EmotionConfig {
                enabled: e.enabled.unwrap_or(false),
                intensity_importance_k: e.intensity_importance_k.unwrap_or(3.0),
                blend_rate: e.blend_rate.unwrap_or(0.3),
                decay_half_life_hours: e.decay_half_life_hours.unwrap_or(12.0),
            }).unwrap_or_default(),
```

Ensure the `use crate::config::{... SoulConfig, DriftConfig, InitiativeConfig, EmotionConfig}` import (already present in `build_agent_config`) stays.

**Required:** extend the test helper `minimal_payload` (schema.rs `mod tests`, ~356) with the four new fields so it still compiles — add `soul: None, drift: None, initiative: None, emotion: None,` alongside the existing `compaction: None`.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p opex-core --bin opex-core build_agent_config_maps_soul_payload build_agent_config_absent_soul_is_default`
Expected: PASS (2). Run existing schema tests: `cargo test -p opex-core --bin opex-core schema::` → PASS.

- [ ] **Step 5: clippy + commit**

Run: `cargo clippy -p opex-core --bin opex-core` → clean.

```bash
git add crates/opex-core/src/gateway/handlers/agents/schema.rs
git commit -m "feat(agents): accept soul/drift/initiative/emotion in create/update payload"
```

---

### Task 5: Presence-gated preserve-merge (regression guard on a44a4a53)

Once the four sections have payload fields, the current unconditional preserve-from-disk would discard UI edits. Change it so: `delegation` stays unconditionally preserved; the four soul sections are preserved from disk ONLY when the payload omitted the section (outer `Option` is `None`); when present (UI sent the key, even as `null`), the built value wins.

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/crud.rs` (`preserve_no_payload_sections` + new merge helper + update handler wiring; capture presence before `build_agent_config` consumes payload at ~736)
- Test: `crud.rs` `mod tests`

**Interfaces:**
- Consumes: `AgentCreatePayload` fields from Task 4.
- Produces: `struct SoulSectionPresence { soul: bool, drift: bool, initiative: bool, emotion: bool }` and `fn merge_soul_sections(new: &mut AgentConfig, existing: &AgentConfig, present: SoulSectionPresence)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn merge_soul_sections_absent_preserves_disk_present_takes_ui() {
    use super::{merge_soul_sections, SoulSectionPresence};
    let existing: AgentConfig = toml::from_str(
        "[agent]\nname=\"T\"\nprovider=\"openai\"\nmodel=\"gpt-4o\"\n\
         [agent.soul]\nenabled=true\n[agent.drift]\nenabled=true\n",
    ).unwrap();

    // soul omitted in payload → preserve disk (enabled); drift present → UI wins (disabled)
    let mut new_cfg: AgentConfig = toml::from_str(
        "[agent]\nname=\"T\"\nprovider=\"openai\"\nmodel=\"gpt-4o\"\n",
    ).unwrap();
    merge_soul_sections(&mut new_cfg, &existing, SoulSectionPresence {
        soul: false, drift: true, initiative: false, emotion: false,
    });
    assert!(new_cfg.agent.soul.enabled, "soul omitted → preserved from disk");
    assert!(!new_cfg.agent.drift.enabled, "drift present → UI value (disabled) wins");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core merge_soul_sections_absent_preserves_disk_present_takes_ui`
Expected: FAIL — `merge_soul_sections` / `SoulSectionPresence` not found.

- [ ] **Step 3: Rework the helper**

Replace `preserve_no_payload_sections` so it only handles delegation, and add the merge helper + presence struct:

```rust
/// Preserve config sections that have NO payload field on PUT. Currently only
/// `[agent.delegation]` (operator-level, TOML-only). See PR #24 review C5.
pub(crate) fn preserve_no_payload_sections(
    new: &mut crate::config::AgentConfig,
    existing: &crate::config::AgentConfig,
) {
    new.agent.delegation = existing.agent.delegation.clone();
}

/// Which soul-layer payload sections were present in the raw PUT body (outer
/// `Option` is `Some`, regardless of null/value inside). Computed BEFORE
/// `build_agent_config` consumes the payload.
pub(crate) struct SoulSectionPresence {
    pub soul: bool,
    pub drift: bool,
    pub initiative: bool,
    pub emotion: bool,
}

/// For each soul-layer section: if the payload omitted it, keep the on-disk
/// value (no silent wipe); if present, keep the freshly-built value (UI wins).
pub(crate) fn merge_soul_sections(
    new: &mut crate::config::AgentConfig,
    existing: &crate::config::AgentConfig,
    present: SoulSectionPresence,
) {
    if !present.soul { new.agent.soul = existing.agent.soul.clone(); }
    if !present.drift { new.agent.drift = existing.agent.drift.clone(); }
    if !present.initiative { new.agent.initiative = existing.agent.initiative.clone(); }
    if !present.emotion { new.agent.emotion = existing.agent.emotion.clone(); }
}
```

- [ ] **Step 4: Wire into the update handler**

Before `build_agent_config` consumes the payload (~736, alongside the existing `payload_webhooks_present` capture), add:

```rust
let soul_presence = SoulSectionPresence {
    soul: payload.soul.is_some(),
    drift: payload.drift.is_some(),
    initiative: payload.initiative.is_some(),
    emotion: payload.emotion.is_some(),
};
```

Then, where the old code called `preserve_no_payload_sections(&mut cfg, &existing_cfg);` (~743), keep that call (delegation) and add immediately after:

```rust
merge_soul_sections(&mut cfg, &existing_cfg, soul_presence);
```

- [ ] **Step 5: Run tests + verify old preserve test still holds**

Run: `cargo test -p opex-core --bin opex-core merge_soul_sections_absent_preserves_disk_present_takes_ui`
Expected: PASS.
Run the earlier guard test name if still present, plus: `cargo test -p opex-core --bin opex-core agents::crud::tests` (DB-gated failures excepted).
Run: `cargo clippy -p opex-core --bin opex-core` → clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/agents/crud.rs
git commit -m "fix(agents): presence-gated merge of soul sections (UI edits win, omitted preserved)"
```

---

### Task 6: UI form state — page.tsx

Extend the form model + GET→form + form→payload mapping in the parent page. No visual changes yet.

**Files:**
- Modify: `ui/src/app/(authenticated)/agents/page.tsx` (`FormState` interface, `emptyForm`, `detailToForm`, `formToPayload`)
- Test: `ui/src/app/(authenticated)/agents/__tests__/agent-form.test.tsx` (existing form test file)

**Interfaces:**
- Consumes: `AgentDetail.soul/drift/initiative/emotion` (Task 3 generated types).
- Produces: `FormState` fields `soulEnabled`, `soulReflectionThreshold`, `soulCooldownMin`, `soulTopK`, `soulBudgetTokens`, `soulMaxEvents`, `driftEnabled`, `driftThreshold`, `driftMinHistory`, `driftBaselineTurns`, `driftCorrect`, `driftAnchor`, `initiativeEnabled`, `initiativeProposalCap`, `initiativeDecompose`, `initiativeDailyPlan`, `initiativeAutoApprove`, `initiativeTokenBudget`, `emotionEnabled`, `emotionK`, `emotionBlendRate`, `emotionHalfLife`. Consumed by Task 7 (the tab UI).

- [ ] **Step 1: Write the failing test**

Add to `agent-form.test.tsx` (import `detailToForm`, `formToPayload` from `../page` — match existing import style):

```tsx
it("round-trips the soul layer through detailToForm and formToPayload", () => {
  const detail: any = {
    name: "T", language: "ru", profile: "default", temperature: 1,
    capabilities: {}, routing: [], daily_budget_tokens: 0, max_failover_attempts: 3,
    is_running: false, config_dirty: false,
    soul: { enabled: true, reflection_threshold: 150, reflection_cooldown_minutes: 60,
            context_top_k: 6, context_budget_tokens: 800, max_events_per_session: 10 },
    drift: { enabled: true, threshold: 0.15, min_history: 6, baseline_turns: 3, correct: true, anchor: "You are T." },
    initiative: { enabled: true, daily_proposal_cap: 1, decompose: false, daily_plan: true,
                  auto_approve_day_plan: false, daily_token_budget: 0 },
    emotion: { enabled: true, intensity_importance_k: 3, blend_rate: 0.3, decay_half_life_hours: 12 },
  };
  const form = detailToForm(detail);
  expect(form.soulEnabled).toBe(true);
  expect(form.driftCorrect).toBe(true);
  expect(form.driftAnchor).toBe("You are T.");
  const payload: any = formToPayload(form);
  expect(payload.soul.enabled).toBe(true);
  expect(payload.drift.anchor).toBe("You are T.");
  expect(payload.emotion.enabled).toBe(true);
});

it("sends null-ish soul sections as disabled objects, not omitted", () => {
  const payload: any = formToPayload({ ...emptyForm });
  expect(payload.soul).toEqual(expect.objectContaining({ enabled: false }));
});
```

(Import `emptyForm` too.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ui && npx vitest run src/app/\(authenticated\)/agents/__tests__/agent-form.test.tsx`
Expected: FAIL — `form.soulEnabled` undefined / `payload.soul` undefined.

- [ ] **Step 3: Extend FormState + emptyForm**

Add to the `FormState` interface (flat fields; numbers as `string`):

```ts
  soulEnabled: boolean; soulReflectionThreshold: string; soulCooldownMin: string;
  soulTopK: string; soulBudgetTokens: string; soulMaxEvents: string;
  driftEnabled: boolean; driftThreshold: string; driftMinHistory: string;
  driftBaselineTurns: string; driftCorrect: boolean; driftAnchor: string;
  initiativeEnabled: boolean; initiativeProposalCap: string; initiativeDecompose: boolean;
  initiativeDailyPlan: boolean; initiativeAutoApprove: boolean; initiativeTokenBudget: string;
  emotionEnabled: boolean; emotionK: string; emotionBlendRate: string; emotionHalfLife: string;
```

Add to `emptyForm` (defaults from the ranges table):

```ts
  soulEnabled: false, soulReflectionThreshold: "150", soulCooldownMin: "60",
  soulTopK: "6", soulBudgetTokens: "800", soulMaxEvents: "10",
  driftEnabled: false, driftThreshold: "0.15", driftMinHistory: "6",
  driftBaselineTurns: "3", driftCorrect: false, driftAnchor: "",
  initiativeEnabled: false, initiativeProposalCap: "1", initiativeDecompose: false,
  initiativeDailyPlan: false, initiativeAutoApprove: false, initiativeTokenBudget: "0",
  emotionEnabled: false, emotionK: "3", emotionBlendRate: "0.3", emotionHalfLife: "12",
```

- [ ] **Step 4: Extend detailToForm**

Add to the returned object in `detailToForm`:

```ts
    soulEnabled: d.soul?.enabled ?? false,
    soulReflectionThreshold: String(d.soul?.reflection_threshold ?? 150),
    soulCooldownMin: String(d.soul?.reflection_cooldown_minutes ?? 60),
    soulTopK: String(d.soul?.context_top_k ?? 6),
    soulBudgetTokens: String(d.soul?.context_budget_tokens ?? 800),
    soulMaxEvents: String(d.soul?.max_events_per_session ?? 10),
    driftEnabled: d.drift?.enabled ?? false,
    driftThreshold: String(d.drift?.threshold ?? 0.15),
    driftMinHistory: String(d.drift?.min_history ?? 6),
    driftBaselineTurns: String(d.drift?.baseline_turns ?? 3),
    driftCorrect: d.drift?.correct ?? false,
    driftAnchor: d.drift?.anchor ?? "",
    initiativeEnabled: d.initiative?.enabled ?? false,
    initiativeProposalCap: String(d.initiative?.daily_proposal_cap ?? 1),
    initiativeDecompose: d.initiative?.decompose ?? false,
    initiativeDailyPlan: d.initiative?.daily_plan ?? false,
    initiativeAutoApprove: d.initiative?.auto_approve_day_plan ?? false,
    initiativeTokenBudget: String(d.initiative?.daily_token_budget ?? 0),
    emotionEnabled: d.emotion?.enabled ?? false,
    emotionK: String(d.emotion?.intensity_importance_k ?? 3),
    emotionBlendRate: String(d.emotion?.blend_rate ?? 0.3),
    emotionHalfLife: String(d.emotion?.decay_half_life_hours ?? 12),
```

- [ ] **Step 5: Extend formToPayload**

Add these keys to the returned payload object (always send the key so presence-merge treats UI as authoritative; numbers parsed; empty anchor → null):

```ts
    soul: {
      enabled: f.soulEnabled,
      reflection_threshold: parseFloat(f.soulReflectionThreshold) || 150,
      reflection_cooldown_minutes: parseInt(f.soulCooldownMin) || 60,
      context_top_k: parseInt(f.soulTopK) || 6,
      context_budget_tokens: parseInt(f.soulBudgetTokens) || 800,
      max_events_per_session: parseInt(f.soulMaxEvents) || 10,
    },
    drift: {
      enabled: f.driftEnabled,
      threshold: parseFloat(f.driftThreshold) || 0.15,
      min_history: parseInt(f.driftMinHistory) || 6,
      baseline_turns: parseInt(f.driftBaselineTurns) || 3,
      correct: f.driftCorrect,
      anchor: f.driftAnchor.trim() !== "" ? f.driftAnchor : null,
    },
    initiative: {
      enabled: f.initiativeEnabled,
      daily_proposal_cap: parseInt(f.initiativeProposalCap) || 1,
      decompose: f.initiativeDecompose,
      daily_plan: f.initiativeDailyPlan,
      auto_approve_day_plan: f.initiativeAutoApprove,
      daily_token_budget: parseInt(f.initiativeTokenBudget) || 0,
    },
    emotion: {
      enabled: f.emotionEnabled,
      intensity_importance_k: parseFloat(f.emotionK) || 3,
      blend_rate: parseFloat(f.emotionBlendRate) || 0.3,
      decay_half_life_hours: parseFloat(f.emotionHalfLife) || 12,
    },
```

- [ ] **Step 6: Run test to verify pass**

Run: `cd ui && npx vitest run src/app/\(authenticated\)/agents/__tests__/agent-form.test.tsx`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add "ui/src/app/(authenticated)/agents/page.tsx" "ui/src/app/(authenticated)/agents/__tests__/agent-form.test.tsx"
git commit -m "feat(ui): map soul layer through agent form state (page.tsx)"
```

---

### Task 7: UI — "Soul" tab in AgentEditDialog

Add the tab, its panel with four `SwitchSection`s, a reusable "Advanced" collapsible, a textarea for the drift anchor, and the cross-field gating.

**Files:**
- Modify: `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` (import `Sparkles` + `Collapsible`; `AgentTab` union; `AGENT_TABS`; new panel; a local `AdvancedSection` helper)
- Test: `ui/src/app/(authenticated)/agents/__tests__/agent-form.test.tsx` (gating logic — pure helper) OR a new `soul-tab.test.tsx`

**Interfaces:**
- Consumes: `form`/`upd` props (Task 6 fields), `editingBase` prop (`AgentEditDialog.tsx:159`).

- [ ] **Step 1: Write the failing test (gating helper)**

Gating is easiest to test as a pure function. Add an exported helper to `AgentEditDialog.tsx`:

```ts
export function soulGating(form: { soulEnabled: boolean; driftEnabled: boolean; initiativeDailyPlan: boolean }, editingBase: boolean) {
  return {
    emotionDisabled: !form.soulEnabled,
    driftCorrectDisabled: !form.driftEnabled,
    autoApproveDisabled: !form.initiativeDailyPlan,
    initiativeDisabled: editingBase,
  };
}
```

Test (`agent-form.test.tsx`):

```tsx
import { soulGating } from "../AgentEditDialog";
it("gates soul cross-fields", () => {
  expect(soulGating({ soulEnabled: false, driftEnabled: false, initiativeDailyPlan: false }, false))
    .toEqual({ emotionDisabled: true, driftCorrectDisabled: true, autoApproveDisabled: true, initiativeDisabled: false });
  expect(soulGating({ soulEnabled: true, driftEnabled: true, initiativeDailyPlan: true }, true))
    .toEqual({ emotionDisabled: false, driftCorrectDisabled: false, autoApproveDisabled: false, initiativeDisabled: true });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ui && npx vitest run src/app/\(authenticated\)/agents/__tests__/agent-form.test.tsx`
Expected: FAIL — `soulGating` not exported.

- [ ] **Step 3: Add the gating helper + tab wiring**

- Add `soulGating` (from Step 1) to `AgentEditDialog.tsx`.
- Import: add `Sparkles` to the `lucide-react` import (`:41`) and `Collapsible, CollapsibleTrigger, CollapsibleContent` from `@/components/ui/collapsible`.
- Extend `AgentTab`: `... | "soul"` (`:162`).
- Add to `AGENT_TABS` (`:164-171`): `{ id: "soul", icon: Sparkles, labelKey: "agents.tab_soul" }`.

- [ ] **Step 4: Add the panel + AdvancedSection helper**

Add a local helper near `SwitchSection` (`:742`):

```tsx
function AdvancedSection({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <Collapsible className="mt-2">
      <CollapsibleTrigger className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
        <ChevronDown className="h-3 w-3" /> {label}
      </CollapsibleTrigger>
      <CollapsibleContent className="space-y-2 pt-2">{children}</CollapsibleContent>
    </Collapsible>
  );
}
```

Add the panel inside the tab grid (mirror the `activeTab === "behavior"` panel at `:451`). Full Soul section shown; build Drift/Initiative/Emotion identically using the `FormState` fields from Task 6 and the ranges from the spec table for `min`/`max`:

```tsx
<div className={`col-start-1 row-start-1 space-y-3 transition-none ${activeTab === "soul" ? "" : "opacity-0 pointer-events-none select-none"}`}>
  {(() => { const g = soulGating(form, !!editingBase); return (<>
    <SwitchSection title={t("agents.section_soul")} enabled={form.soulEnabled} onToggle={(v) => upd({ soulEnabled: v })}>
      <AdvancedSection label={t("common.advanced")}>
        <Field label={t("agents.soul_reflection_threshold")} labelClassName="text-xs">
          <Input type="number" min={1} value={form.soulReflectionThreshold} onChange={(e) => upd({ soulReflectionThreshold: e.target.value })} />
        </Field>
        <Field label={t("agents.soul_cooldown_min")} labelClassName="text-xs">
          <Input type="number" min={0} max={1440} value={form.soulCooldownMin} onChange={(e) => upd({ soulCooldownMin: e.target.value })} />
        </Field>
        <Field label={t("agents.soul_top_k")} labelClassName="text-xs">
          <Input type="number" min={1} max={20} value={form.soulTopK} onChange={(e) => upd({ soulTopK: e.target.value })} />
        </Field>
        <Field label={t("agents.soul_budget_tokens")} labelClassName="text-xs">
          <Input type="number" min={100} max={4000} value={form.soulBudgetTokens} onChange={(e) => upd({ soulBudgetTokens: e.target.value })} />
        </Field>
        <Field label={t("agents.soul_max_events")} labelClassName="text-xs">
          <Input type="number" min={1} max={30} value={form.soulMaxEvents} onChange={(e) => upd({ soulMaxEvents: e.target.value })} />
        </Field>
      </AdvancedSection>
    </SwitchSection>

    <SwitchSection title={t("agents.section_drift")} enabled={form.driftEnabled} onToggle={(v) => upd({ driftEnabled: v })}>
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-muted-foreground">{t("agents.drift_correct")}</span>
        <Switch checked={form.driftCorrect} disabled={g.driftCorrectDisabled} onCheckedChange={(v) => upd({ driftCorrect: v })} />
      </div>
      <Field label={t("agents.drift_anchor")} labelClassName="text-xs" hint={t("agents.drift_anchor_hint")}>
        <textarea className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm" rows={2}
          value={form.driftAnchor} onChange={(e) => upd({ driftAnchor: e.target.value })} />
      </Field>
      <AdvancedSection label={t("common.advanced")}>
        <Field label={t("agents.drift_threshold")} labelClassName="text-xs">
          <Input type="number" step="0.01" min={0} max={2} value={form.driftThreshold} onChange={(e) => upd({ driftThreshold: e.target.value })} />
        </Field>
        <Field label={t("agents.drift_min_history")} labelClassName="text-xs">
          <Input type="number" min={2} max={50} value={form.driftMinHistory} onChange={(e) => upd({ driftMinHistory: e.target.value })} />
        </Field>
        <Field label={t("agents.drift_baseline_turns")} labelClassName="text-xs">
          <Input type="number" min={1} max={10} value={form.driftBaselineTurns} onChange={(e) => upd({ driftBaselineTurns: e.target.value })} />
        </Field>
      </AdvancedSection>
    </SwitchSection>

    <SwitchSection title={t("agents.section_initiative")} enabled={form.initiativeEnabled}
      onToggle={(v) => upd({ initiativeEnabled: v })}>
      {g.initiativeDisabled && <p className="text-xs text-warning">{t("agents.initiative_non_base_note")}</p>}
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-muted-foreground">{t("agents.initiative_daily_plan")}</span>
        <Switch checked={form.initiativeDailyPlan} disabled={g.initiativeDisabled} onCheckedChange={(v) => upd({ initiativeDailyPlan: v })} />
      </div>
      <p className="text-xs text-muted-foreground">{t("agents.initiative_daily_plan_hint")}</p>
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-muted-foreground">{t("agents.initiative_auto_approve")}</span>
        <Switch checked={form.initiativeAutoApprove} disabled={g.autoApproveDisabled || g.initiativeDisabled} onCheckedChange={(v) => upd({ initiativeAutoApprove: v })} />
      </div>
      <AdvancedSection label={t("common.advanced")}>
        <Field label={t("agents.initiative_proposal_cap")} labelClassName="text-xs">
          <Input type="number" min={1} max={10} disabled={g.initiativeDisabled} value={form.initiativeProposalCap} onChange={(e) => upd({ initiativeProposalCap: e.target.value })} />
        </Field>
        <div className="flex items-center justify-between">
          <span className="text-xs font-medium text-muted-foreground">{t("agents.initiative_decompose")}</span>
          <Switch checked={form.initiativeDecompose} disabled={g.initiativeDisabled} onCheckedChange={(v) => upd({ initiativeDecompose: v })} />
        </div>
        <Field label={t("agents.initiative_token_budget")} labelClassName="text-xs" hint={t("agents.initiative_token_budget_hint")}>
          <Input type="number" min={0} max={1000000000000} disabled={g.initiativeDisabled} value={form.initiativeTokenBudget} onChange={(e) => upd({ initiativeTokenBudget: e.target.value })} />
        </Field>
      </AdvancedSection>
    </SwitchSection>

    <SwitchSection title={t("agents.section_emotion")} enabled={form.emotionEnabled}
      onToggle={(v) => { if (v && g.emotionDisabled) return; upd({ emotionEnabled: v }); }}>
      {g.emotionDisabled && <p className="text-xs text-warning">{t("agents.emotion_requires_soul_note")}</p>}
      <AdvancedSection label={t("common.advanced")}>
        <Field label={t("agents.emotion_k")} labelClassName="text-xs">
          <Input type="number" step="0.1" min={0} max={5} value={form.emotionK} onChange={(e) => upd({ emotionK: e.target.value })} />
        </Field>
        <Field label={t("agents.emotion_blend_rate")} labelClassName="text-xs">
          <Input type="number" step="0.05" min={0} max={1} value={form.emotionBlendRate} onChange={(e) => upd({ emotionBlendRate: e.target.value })} />
        </Field>
        <Field label={t("agents.emotion_half_life")} labelClassName="text-xs">
          <Input type="number" step="0.5" min={0} value={form.emotionHalfLife} onChange={(e) => upd({ emotionHalfLife: e.target.value })} />
        </Field>
      </AdvancedSection>
    </SwitchSection>
  </>); })()}
</div>
```

Note: the Emotion `SwitchSection` guards its own enable when soul is off (the `onToggle` early-return); the note explains why. Because `SwitchSection` hides children when disabled, the emotion-requires-soul note renders only after emotion is on — acceptable; the primary guard is the onToggle + server 400.

- [ ] **Step 5: Run test to verify pass**

Run: `cd ui && npx vitest run src/app/\(authenticated\)/agents/__tests__/agent-form.test.tsx`
Expected: PASS (gating test).
Also run the build type-check: `cd ui && npx tsc --noEmit` — expect no errors from the new panel (fix any missing import).

- [ ] **Step 6: Commit**

```bash
git add "ui/src/app/(authenticated)/agents/AgentEditDialog.tsx" "ui/src/app/(authenticated)/agents/__tests__/agent-form.test.tsx"
git commit -m "feat(ui): Soul tab in agent editor (soul/drift/initiative/emotion + gating)"
```

---

### Task 8: i18n keys (en + ru)

Add every `t("agents.*")` / `t("common.advanced")` key referenced in Task 7 to both locale files.

**Files:**
- Modify: `ui/src/i18n/locales/en.json`
- Modify: `ui/src/i18n/locales/ru.json`

- [ ] **Step 1: Add keys to en.json**

Add (flat keys, near the other `agents.*` keys):

```json
"agents.tab_soul": "Soul",
"agents.section_soul": "Autobiographical memory",
"agents.soul_reflection_threshold": "Reflection threshold",
"agents.soul_cooldown_min": "Reflection cooldown (min)",
"agents.soul_top_k": "Context top-K",
"agents.soul_budget_tokens": "Context budget (tokens)",
"agents.soul_max_events": "Max events per session",
"agents.section_drift": "Persona drift",
"agents.drift_correct": "Inject identity anchor on drift",
"agents.drift_anchor": "Identity anchor",
"agents.drift_anchor_hint": "1–2 sentence operator reminder of who this agent is.",
"agents.drift_threshold": "Drift threshold",
"agents.drift_min_history": "Min history turns",
"agents.drift_baseline_turns": "Baseline turns",
"agents.section_initiative": "Proactive initiative",
"agents.initiative_non_base_note": "Initiative is available for non-base agents only.",
"agents.initiative_daily_plan": "Daily plan",
"agents.initiative_daily_plan_hint": "Requires a configured heartbeat (Schedule tab) and enabled initiative.",
"agents.initiative_auto_approve": "Auto-approve daily plan",
"agents.initiative_proposal_cap": "Daily proposal cap",
"agents.initiative_decompose": "Decompose goals",
"agents.initiative_token_budget": "Daily token budget",
"agents.initiative_token_budget_hint": "Advancement pauses for the day once usage reaches this. Required when auto-approve is on.",
"agents.section_emotion": "Mood",
"agents.emotion_requires_soul_note": "Requires the Soul (autobiographical memory) section enabled.",
"agents.emotion_k": "Intensity → importance factor",
"agents.emotion_blend_rate": "Mood blend rate",
"agents.emotion_half_life": "Mood decay half-life (hours)",
"common.advanced": "Advanced",
```

- [ ] **Step 2: Add the same keys to ru.json (Russian values)**

```json
"agents.tab_soul": "Душа",
"agents.section_soul": "Автобиографическая память",
"agents.soul_reflection_threshold": "Порог рефлексии",
"agents.soul_cooldown_min": "Кулдаун рефлексии (мин)",
"agents.soul_top_k": "Контекст top-K",
"agents.soul_budget_tokens": "Бюджет контекста (токены)",
"agents.soul_max_events": "Макс. событий за сессию",
"agents.section_drift": "Дрейф личности",
"agents.drift_correct": "Вставлять якорь личности при дрейфе",
"agents.drift_anchor": "Якорь личности",
"agents.drift_anchor_hint": "1–2 предложения-напоминание, кто этот агент.",
"agents.drift_threshold": "Порог дрейфа",
"agents.drift_min_history": "Мин. ходов истории",
"agents.drift_baseline_turns": "Опорных ходов",
"agents.section_initiative": "Проактивная инициатива",
"agents.initiative_non_base_note": "Инициатива доступна только не-base агентам.",
"agents.initiative_daily_plan": "Дневной план",
"agents.initiative_daily_plan_hint": "Требует настроенный heartbeat (вкладка «Расписание») и включённую инициативу.",
"agents.initiative_auto_approve": "Авто-одобрение дневного плана",
"agents.initiative_proposal_cap": "Лимит предложений в день",
"agents.initiative_decompose": "Декомпозировать цели",
"agents.initiative_token_budget": "Дневной бюджет токенов",
"agents.initiative_token_budget_hint": "Продвижение встаёт на паузу на день по достижении лимита. Обязателен при авто-одобрении.",
"agents.section_emotion": "Настроение",
"agents.emotion_requires_soul_note": "Требует включённую секцию «Душа» (автобиографическая память).",
"agents.emotion_k": "Коэффициент интенсивность → важность",
"agents.emotion_blend_rate": "Скорость смешения настроения",
"agents.emotion_half_life": "Полураспад настроения (часы)",
"common.advanced": "Дополнительно",
```

- [ ] **Step 3: Verify no missing keys**

Run: `cd ui && npx tsc --noEmit` (if the i18n typing surfaces missing keys) and `cd ui && npx vitest run` for the agents form tests. Manually confirm both files parse: `node -e "JSON.parse(require('fs').readFileSync('ui/src/i18n/locales/en.json'))"` and same for ru.

- [ ] **Step 4: Commit**

```bash
git add ui/src/i18n/locales/en.json ui/src/i18n/locales/ru.json
git commit -m "i18n(agents): soul-layer editor labels (en+ru)"
```

---

### Task 9: README differentiator row (EN + RU)

Add one factual row to the "What's actually different" table. No "soul"/"consciousness" wording.

**Files:**
- Modify: `README.md` (table at `## What's actually different`, ~47-56)
- Modify: `README.ru.md` (mirror table)

- [ ] **Step 1: Add the row to README.md**

After the "A fleet, not a bot" row (`README.md:56`), add:

```markdown
| **Agents that carry context forward** | Opt-in, per-agent, default-off: an agent can keep an autobiographical memory — session events distilled into periodic reflections and a self-portrait file — and propose its own next steps between conversations (a daily plan advanced on a heartbeat, one owner tap to approve). Persona-drift detection can nudge an agent back toward its configured identity. All configured from the agent editor, all stored as inspectable rows and Markdown, none of it on by default. |
```

- [ ] **Step 2: Add the mirrored row to README.ru.md**

Add the Russian mirror after the corresponding row:

```markdown
| **Агенты, переносящие контекст вперёд** | Опционально, по-агентно, по умолчанию выключено: агент может вести автобиографическую память — события сессий сжимаются в периодические рефлексии и файл-автопортрет — и предлагать собственные следующие шаги между разговорами (дневной план, продвигаемый по heartbeat, одобрение владельца одним тапом). Детекция дрейфа личности мягко возвращает агента к настроенной идентичности. Всё настраивается из редактора агента, хранится как инспектируемые записи и Markdown, ничего не включено по умолчанию. |
```

- [ ] **Step 3: Verify**

Manual: open both READMEs, confirm the row renders inside the table (pipe alignment) and no "soul/душа" wording is used.

- [ ] **Step 4: Commit**

```bash
git add README.md README.ru.md
git commit -m "docs(readme): document opt-in per-agent autobiographical memory + initiative"
```

---

### Task 10: End-to-end verification + deploy

**Files:** none (verification only).

- [ ] **Step 1: Full Rust test + clippy**

Run: `cargo test -p opex-core --bin opex-core agents::` and `cargo clippy -p opex-core --bin opex-core` → clean (DB-gated `#[sqlx::test]` failures excepted).

- [ ] **Step 2: UI tests + typecheck + build**

Run: `cd ui && npx vitest run` (agents form/gating tests green) and `cd ui && npm run build` → success.

- [ ] **Step 3: Regenerate + verify no type drift**

Run: `cargo run --features ts-gen --bin gen_ts_types -p opex-core` then `git diff --stat ui/src/types/api.generated.ts` → expect no uncommitted changes (already committed in Task 3).

- [ ] **Step 4: Manual smoke (local or after deploy)**

Open the agent editor → Soul tab → enable Soul, set reflection threshold, enable Drift with an anchor, save → reopen → values persisted. Toggle another field (e.g. temperature) on a soul-enabled agent, save → confirm soul stays enabled (the a44a4a53 + Task 5 guarantee). Try enabling Emotion without Soul → blocked client-side; if forced, server returns 400.

- [ ] **Step 5: Deploy (only on explicit user approval)**

Per project rule, deploy needs explicit go-ahead. When approved: push `master`, then run the server deploy (`ssh aronmav@188.246.224.118 'bash ~/opex-src/scripts/server-deploy.sh'`), then health check.
