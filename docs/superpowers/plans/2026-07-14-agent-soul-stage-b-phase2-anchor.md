# Stage B Phase 2 — A-Anchor Drift Correction — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When persona drift crosses the threshold, inject a compact identity anchor into the next turn's system prompt to pull the model back toward its persona — opt-in, default off.

**Architecture:** Two pure functions in `drift/mod.rs` (anchor text + correction decision) drive the change. `drift_probe` (already run each turn in `context_builder.rs`) changes its return type from `()` to `Option<String>` — the ready anchor block when correction fires — and the call site appends it to the tail of the assembled system prompt. Reuses the existing drift metric, baseline cache, and timeline logging unchanged.

**Tech Stack:** Rust 2024, crate `opex-core`. No new dependencies, no migration.

**Spec:** `docs/superpowers/specs/2026-07-14-agent-soul-stage-b-phase2-anchor-design.md`

## Global Constraints

- **Opt-in, default off** (`correct = false`). Detect-only behaviour is byte-for-byte unchanged when `correct = false`.
- **`correct = true` requires `enabled = true`** (validation error otherwise).
- **Anchor is TRUSTED** (operator config string / agent name) — NOT sanitized.
- **Injection only on over-threshold turns** → most turns are byte-identical to today (cache-friendly); **no new prompt-cache breakpoint**.
- **No ECP** (response re-projection) — out of scope.
- **No change** to the drift metric, baseline cache, or `threshold`/`min_history`/`baseline_turns` semantics.
- `drift_probe` return type changes `()` → `Option<String>`; it is a `ContextBuilderDeps` trait method (declared `context_builder.rs:177`) with a single impl (`engine/context_builder.rs`).
- **Platform:** `cargo check` + `cargo clippy -p opex-core --all-targets -- -D warnings` locally (Windows); `opex-core` unit tests are in the bin target (`cargo test -p opex-core --bin opex-core <filter>`), authoritatively run on the server.
- master, one commit per task, NO `Co-Authored-By`.
- **Do NOT enable on any agent** — shipping default-off; enabling `correct=true` waits for a clean drift-observation window on the restored Opex (rollout, not code).

---

### Task 1: Config fields + pure anchor/decision functions

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` — `DriftConfig` struct (~1432-1441), `Default` impl (~1453-1462), `validate()` (~1464+). Add tests near it.
- Modify: `crates/opex-core/src/agent/drift/mod.rs` — add `build_anchor_block` + `correction_anchor` pure fns + unit tests.

**Interfaces:**
- Produces:
  - `DriftConfig { correct: bool, anchor: Option<String>, .. }` — read by Task 2 as `cfg.correct` / `cfg.anchor`.
  - `drift::build_anchor_block(anchor: Option<&str>, agent_name: &str) -> String`
  - `drift::correction_anchor(score: f32, threshold: f32, correct: bool, anchor: Option<&str>, agent_name: &str) -> Option<String>`

- [ ] **Step 1: Write the failing drift unit tests**

Add to the `#[cfg(test)] mod tests` in `crates/opex-core/src/agent/drift/mod.rs`:

```rust
    #[test]
    fn anchor_uses_operator_string_or_falls_back() {
        let a = build_anchor_block(Some("Ты — Опекс, инфра-ассистент."), "Opex");
        assert!(a.contains("Опекс, инфра-ассистент"));
        assert!(a.contains("[Идентичность — напоминание]"));
        // blank/None → generic fallback naming the agent
        let f = build_anchor_block(None, "Arty");
        assert!(f.contains("Arty"));
        assert!(f.contains("[Идентичность — напоминание]"));
        let b = build_anchor_block(Some("   "), "Arty");
        assert!(b.contains("Arty"), "blank anchor → fallback");
    }

    #[test]
    fn correction_anchor_gates_on_correct_and_threshold() {
        // over threshold + correct → Some(block)
        assert!(correction_anchor(0.5, 0.15, true, None, "A").is_some());
        // over threshold but correct off → None (detect-only)
        assert!(correction_anchor(0.5, 0.15, false, None, "A").is_none());
        // under threshold + correct → None
        assert!(correction_anchor(0.10, 0.15, true, None, "A").is_none());
        // exactly at threshold → None (strict >, matches drift_probe's `score > threshold`)
        assert!(correction_anchor(0.15, 0.15, true, None, "A").is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p opex-core --bin opex-core drift::tests::anchor drift::tests::correction_anchor`
Expected: FAIL — `build_anchor_block` / `correction_anchor` not defined.

- [ ] **Step 3: Add the two pure functions**

In `crates/opex-core/src/agent/drift/mod.rs`, after `drift_score` (line ~50):

```rust
/// Compact identity-reminder block appended to the system prompt on an
/// over-threshold turn. Operator's `anchor` when set/non-blank, else a generic
/// name-based fallback. Trusted input (operator config / agent name) — not sanitized.
pub fn build_anchor_block(anchor: Option<&str>, agent_name: &str) -> String {
    let body = match anchor {
        Some(a) if !a.trim().is_empty() => a.trim().to_string(),
        _ => format!("Ты — {agent_name}. Сохраняй свой характер, тон и манеру речи."),
    };
    format!("\n\n[Идентичность — напоминание]\n{body}\n")
}

/// Correction decision: the anchor block to inject, or None. Some iff correction
/// is enabled AND the score is strictly over threshold (mirrors drift_probe's
/// `over = score > threshold`).
pub fn correction_anchor(
    score: f32,
    threshold: f32,
    correct: bool,
    anchor: Option<&str>,
    agent_name: &str,
) -> Option<String> {
    if correct && score > threshold {
        Some(build_anchor_block(anchor, agent_name))
    } else {
        None
    }
}
```

- [ ] **Step 4: Run the drift tests to verify they pass**

Run: `cargo test -p opex-core --bin opex-core drift::tests`
Expected: all `drift::tests` PASS (new + existing centroid/drift_score/own_texts).

- [ ] **Step 5: Write the failing config validation test**

Add to `crates/opex-core/src/config/mod.rs` in a `#[cfg(test)] mod drift_config_tests { use super::*;` block after `impl DriftConfig`:

```rust
#[cfg(test)]
mod drift_config_tests {
    use super::*;

    #[test]
    fn correct_requires_enabled() {
        let ok = DriftConfig { enabled: true, correct: true, ..DriftConfig::default() };
        assert!(ok.validate().is_empty(), "correct+enabled must pass: {:?}", ok.validate());

        let bad = DriftConfig { enabled: false, correct: true, ..DriftConfig::default() };
        assert!(bad.validate().iter().any(|e| e.contains("correct")), "correct without enabled must error");

        let off = DriftConfig { enabled: false, correct: false, ..DriftConfig::default() };
        assert!(off.validate().is_empty(), "correct off → no error");
    }
}
```

- [ ] **Step 6: Run the config test to verify it fails**

Run: `cargo test -p opex-core --bin opex-core drift_config_tests`
Expected: FAIL — fields `correct` / `anchor` do not exist yet (compile error).

- [ ] **Step 7: Add the two fields + Default + validation**

In `crates/opex-core/src/config/mod.rs`, add to `pub struct DriftConfig` (after `pub baseline_turns: usize,`):

```rust
    #[serde(default)]
    pub baseline_turns: usize,
    /// Opt-in: inject an identity anchor into the system prompt when drift is
    /// over threshold. Requires `enabled = true`.
    #[serde(default)]
    pub correct: bool,
    /// Operator identity reminder (~1-2 sentences). None → generic name-based fallback.
    #[serde(default)]
    pub anchor: Option<String>,
```

Add to the `Default` impl (after `baseline_turns: default_drift_baseline_turns(),`):

```rust
            baseline_turns: default_drift_baseline_turns(),
            correct: false,
            anchor: None,
```

In `DriftConfig::validate()`, before the final `errors`:

```rust
        if self.correct && !self.enabled {
            errors.push("drift.correct requires drift.enabled = true".to_string());
        }
        errors
```

(Insert the `if` immediately before the function's existing `errors` return; keep the existing threshold/min_history/baseline_turns checks above it.)

- [ ] **Step 8: Run check, clippy, and both test modules**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings` then `cargo test -p opex-core --bin opex-core drift::tests drift_config_tests`
Expected: check + clippy clean; all tests PASS. (Any existing `DriftConfig { .. }` struct literals in other tests that don't use `..DriftConfig::default()` will need the two new fields — fix them if `cargo check` reports them.)

- [ ] **Step 9: Commit**

```bash
git add crates/opex-core/src/config/mod.rs crates/opex-core/src/agent/drift/mod.rs
git commit -m "feat(soul): drift correct/anchor config + pure anchor-block + correction decision"
```

---

### Task 2: Probe returns the anchor; inject into the system prompt

**Files:**
- Modify: `crates/opex-core/src/agent/context_builder.rs` — trait method decl (line 177) `drift_probe` return type; call site (line 294) capture; inject after the todo block (~line 546).
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs` — `drift_probe` impl (lines 302-376): return `Option<String>`, add `corrected` to payload, log on inject.

**Interfaces:**
- Consumes: Task 1 `cfg.correct` / `cfg.anchor` and `drift::correction_anchor(...)`.
- Produces: `ContextBuilderDeps::drift_probe(&self, &[MessageRow], Uuid) -> Option<String>`.

- [ ] **Step 1: Change the trait method return type**

In `crates/opex-core/src/agent/context_builder.rs:177`, change:

```rust
    async fn drift_probe(&self, history: &[opex_db::sessions::MessageRow], session_id: Uuid);
```

to:

```rust
    /// Persona-drift probe (Stage B). Returns the identity-anchor block to append
    /// to the system prompt when correction fires (`[agent.drift] correct=true`
    /// and drift over threshold), else `None`. Detect-only + timeline logging are
    /// unchanged; the return is `None` on every non-correcting path.
    async fn drift_probe(&self, history: &[opex_db::sessions::MessageRow], session_id: Uuid) -> Option<String>;
```

- [ ] **Step 2: Update the impl to return the anchor + payload flag**

In `crates/opex-core/src/agent/engine/context_builder.rs`, change the impl signature (line 302) to `-> Option<String>` and make every early-return yield `None`. Concretely:

- Line 302: `async fn drift_probe(&self, history: &[opex_db::sessions::MessageRow], session_id: Uuid) -> Option<String> {`
- Every existing `return;` in the body (disabled, too-short history, `texts.len() < baseline_turns+1`, baseline embed err, degenerate centroid, `texts.last()` none, recent embed err) becomes `return None;`.
- After `let over = score > cfg.threshold;` (line 354), compute the anchor and enrich the payload:

```rust
        let over = score > cfg.threshold;
        let anchor = crate::agent::drift::correction_anchor(
            score, cfg.threshold, cfg.correct, cfg.anchor.as_deref(), agent,
        );

        let cos = 1.0 - score;
        let payload = serde_json::json!({
            "drift_score": score,
            "cos_recent_baseline": cos,
            "own_assistant_turns": texts.len(),
            "baseline_turns_used": cfg.baseline_turns,
            "history_len": history.len(),
            "over_threshold": over,
            "corrected": anchor.is_some(),
        });
        if let Err(e) = opex_db::session_timeline::log_event(
            &self.cfg().db, session_id, "drift_probe", Some(&payload),
        ).await {
            tracing::warn!(agent, error = %e, "drift timeline write failed");
        }
        if anchor.is_some() {
            tracing::info!(agent, drift_score = score, "drift anchor injected");
        } else if over {
            tracing::warn!(agent, drift_score = score, "persona drift over threshold");
        } else {
            tracing::debug!(agent, drift_score = score, "drift probe");
        }
        anchor
    }
```

(Replace the existing `if over { warn } else { debug }` tail with the block above; the function now ends `anchor`.)

- [ ] **Step 3: Capture the return at the call site**

In `crates/opex-core/src/agent/context_builder.rs:294`, change:

```rust
        deps.drift_probe(&history, session_id).await;
```

to:

```rust
        let drift_anchor = deps.drift_probe(&history, session_id).await;
```

- [ ] **Step 4: Inject the anchor at the tail of the system prompt**

In `crates/opex-core/src/agent/context_builder.rs`, after the todo block is appended (after `let todo_len = system_prompt.len() - pre_todo_len;`, line ~546) and BEFORE the `system_prompt_size` log (line ~549), append the anchor so it sits at the very end of the system prompt (highest salience):

```rust
        let todo_len = system_prompt.len() - pre_todo_len;

        // Stage B Phase 2: identity anchor at the tail (highest salience) when
        // drift crossed the threshold this turn. Rare (only over-threshold turns).
        if let Some(block) = drift_anchor {
            system_prompt.push_str(&block);
        }
```

- [ ] **Step 5: Run check + clippy**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: both clean. This proves the trait/impl/call-site return types line up, `drift_anchor` is used (no unused-var), and no other `ContextBuilderDeps` impl exists that needs updating (a second impl would fail to compile).

- [ ] **Step 6: Run the drift + context-builder tests**

Run: `cargo test -p opex-core --bin opex-core drift context_builder`
Expected: PASS. The decision logic is covered by Task 1's `correction_anchor` unit tests; existing `context_builder` tests still pass (drift correction is inert when `correct=false`, the default in all fixtures).

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/agent/context_builder.rs crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "feat(soul): inject drift identity-anchor into system prompt on over-threshold turns"
```

---

## Self-Review

**Spec coverage:**
- §3.1 config `correct`/`anchor` + validation → Task 1 Steps 5-7.
- §3.2 pure `build_anchor_block` → Task 1 Steps 1-3.
- §3.3 probe returns `Option<String>` + `corrected` payload + inject-log → Task 2 Steps 1-2.
- §3.4 injection at the call site (tail of system prompt) → Task 2 Steps 3-4.
- §3.5 cache: injection only on over-threshold (rare), no new breakpoint → inherent (append is conditional).
- §6 testing: pure anchor + decision + config validation → Task 1; injection compile/integration → Task 2.
- §7 non-goals honoured: no ECP, no metric change, no hysteresis, no new breakpoint, anchor unsanitized.
- §8 rollout (default off, enable after clean Opex window) → Global Constraints "do not enable".

**Placeholder scan:** none — every code step carries complete code and an exact command.

**Type consistency:** `build_anchor_block(Option<&str>, &str) -> String` and `correction_anchor(f32, f32, bool, Option<&str>, &str) -> Option<String>` (Task 1) are consumed verbatim by `drift_probe` (Task 2). `drift_probe(&self, &[MessageRow], Uuid) -> Option<String>` matches between the trait decl (Task 2 Step 1), the impl (Step 2), and the call-site capture (Step 3). `DriftConfig.correct: bool` / `anchor: Option<String>` defined in Task 1, read in Task 2.
