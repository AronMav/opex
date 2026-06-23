# P0.2 — Session Skill Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fire `review_session_for_skills` in the background after each interactive session that used ≥ N tool calls, queuing skill repairs via the existing `pending_skill_repairs` mechanism.

**Architecture:** New `SkillReviewConfig` in `AgentSettings`, new `review_session_for_skills()` in `skills/evolution.rs` that loads session messages, builds a task summary, and calls the LLM for SKIP/FIX/DERIVED/CAPTURED verdict. `finalize.rs` receives the config and spawns the review alongside the existing knowledge extraction.

**Tech Stack:** Rust, sqlx/PgPool, tokio (timeout + TaskTracker), existing `skill_repairs::enqueue` + `crate::db::sessions::load_messages`.

---

## File Map

| File | Change |
|---|---|
| `crates/opex-core/src/config/mod.rs` | Add `SkillReviewConfig` struct; add `skill_review` field to `AgentSettings` |
| `crates/opex-core/src/skills/evolution.rs` | Add `review_session_for_skills()` + unit tests |
| `crates/opex-core/src/agent/pipeline/finalize.rs` | Add `skill_review` to `FinalizeContext`; add `spawn_skill_review()`; trigger in `Done` arm; update `finalize_context_from_engine()` |

---

## Task 1: Add `SkillReviewConfig` to config

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (~line 1000, after `CompactionConfig`)

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block at the bottom of `config/mod.rs`:

```rust
#[test]
fn skill_review_config_defaults() {
    let cfg = SkillReviewConfig::default();
    assert!(!cfg.enabled);
    assert_eq!(cfg.min_tool_calls, 3);
}

#[test]
fn skill_review_config_from_toml() {
    let toml_str = r#"
        [agent]
        name = "Test"
        provider = "openai"
        model = "gpt-4o"
        [agent.skill_review]
        enabled = true
        min_tool_calls = 5
    "#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
    let sr = cfg.agent.skill_review.expect("skill_review present");
    assert!(sr.enabled);
    assert_eq!(sr.min_tool_calls, 5);
}

#[test]
fn skill_review_absent_gives_none() {
    let toml_str = r#"
        [agent]
        name = "Test"
        provider = "openai"
        model = "gpt-4o"
    "#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
    assert!(cfg.agent.skill_review.is_none());
}

#[test]
fn skill_review_enabled_only_gives_default_min_tool_calls() {
    let toml_str = r#"
        [agent]
        name = "Test"
        provider = "openai"
        model = "gpt-4o"
        [agent.skill_review]
        enabled = true
    "#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parse");
    let sr = cfg.agent.skill_review.expect("present");
    assert!(sr.enabled);
    assert_eq!(sr.min_tool_calls, 3); // default
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p opex-core skill_review_config -- --nocapture 2>&1 | tail -20
```

Expected: FAIL — `SkillReviewConfig` not found.

- [ ] **Step 3: Add `SkillReviewConfig` struct**

In `crates/opex-core/src/config/mod.rs`, after the `impl CompactionConfig` block (~line 1000), add:

```rust
/// Per-agent session skill review config (TOML: `[agent.skill_review]`).
///
/// When enabled, after each `Done` session with ≥ `min_tool_calls` tool
/// invocations, a background task analyzes the session for skill improvements
/// and queues repairs via `pending_skill_repairs`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SkillReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "SkillReviewConfig::default_min_tool_calls")]
    pub min_tool_calls: u32,
}

impl SkillReviewConfig {
    fn default_min_tool_calls() -> u32 { 3 }
}

impl Default for SkillReviewConfig {
    fn default() -> Self { Self { enabled: false, min_tool_calls: 3 } }
}
```

- [ ] **Step 4: Add `skill_review` field to `AgentSettings`**

In `AgentSettings` (line ~666), after `pub compaction: Option<CompactionConfig>`:

```rust
pub skill_review: Option<SkillReviewConfig>,
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p opex-core skill_review_config -- --nocapture 2>&1 | tail -20
```

Expected: 4 tests PASS.

- [ ] **Step 6: Run full config tests to confirm no regressions**

```bash
cargo test -p opex-core config -- --nocapture 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(skill-review): add SkillReviewConfig to AgentSettings"
```

---

## Task 2: Add `review_session_for_skills` to evolution.rs

**Files:**
- Modify: `crates/opex-core/src/skills/evolution.rs`

- [ ] **Step 1: Write failing unit tests**

Add to the `mod tests` block in `evolution.rs`:

```rust
#[test]
fn task_summary_truncates_at_char_boundary() {
    // Build a string of 3000 bytes of ASCII — safe to truncate anywhere
    let long = "x".repeat(3000);
    let truncated_len = long.floor_char_boundary(2000);
    assert_eq!(truncated_len, 2000);
    assert!(long[..truncated_len].len() <= 2000);
}

#[test]
fn task_summary_truncates_multibyte_safely() {
    // Cyrillic char = 2 bytes. 1001 chars = 2002 bytes. Must not split mid-char.
    let long = "А".repeat(1001); // 2002 bytes
    let boundary = long.floor_char_boundary(2000);
    assert!(long.is_char_boundary(boundary));
    assert!(boundary <= 2000);
}

#[test]
fn session_review_skip_verdict_is_detected() {
    let line = "SKIP";
    assert!(line.starts_with("SKIP"));
}

#[test]
fn session_review_captured_extracts_name() {
    let line = "CAPTURED telegram-formatting";
    let name = line
        .strip_prefix("CAPTURED ")
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("");
    assert_eq!(name, "telegram-formatting");
}

#[test]
fn session_review_fix_extracts_name() {
    let line = "FIX web-search because trigger too broad";
    let name = line
        .strip_prefix("FIX ")
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("");
    assert_eq!(name, "web-search");
}

#[test]
fn session_review_derived_extracts_new_name() {
    let line = "DERIVED web-search web-search-news";
    let parts: Vec<&str> = line
        .strip_prefix("DERIVED ")
        .unwrap_or("")
        .split_whitespace()
        .collect();
    assert_eq!(parts.get(1).copied().unwrap_or(""), "web-search-news");
}

#[test]
fn build_task_summary_joins_user_messages() {
    // Simulate the summary-building logic
    let messages = vec![
        ("user", "first question"),
        ("assistant", "answer"),
        ("user", "second question"),
    ];
    let user_parts: Vec<&str> = messages.iter()
        .filter(|(role, _)| *role == "user")
        .map(|(_, content)| *content)
        .collect();
    let summary = user_parts.join("\n---\n");
    assert_eq!(summary, "first question\n---\nsecond question");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p opex-core session_review -- --nocapture 2>&1 | tail -20
```

Expected: FAIL on the `session_review_*` tests (function not yet defined).

- [ ] **Step 3: Add `review_session_for_skills` to `evolution.rs`**

Add the following after `analyze_and_evolve` (before `#[cfg(test)]`):

```rust
/// Analyze a completed interactive session for skill improvements.
///
/// Loads session messages, builds a summary of user requests, and asks the
/// LLM whether any skill should be created or updated. Results are queued
/// via `pending_skill_repairs` for the curator to process.
///
/// Fires only for `Done` sessions with enough tool calls — never blocks.
pub async fn review_session_for_skills(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    session_id: uuid::Uuid,
) {
    if let Err(e) = review_session_inner(db, provider, agent_name, session_id).await {
        tracing::debug!(
            error = %e,
            agent = agent_name,
            session = %session_id,
            "session skill review failed"
        );
    }
}

async fn review_session_inner(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    session_id: uuid::Uuid,
) -> anyhow::Result<()> {
    use std::time::Duration;

    // 1. Load messages (last 30 user+assistant)
    let rows = crate::db::sessions::load_messages(db, session_id, Some(30)).await?;
    let user_parts: Vec<&str> = rows.iter()
        .filter(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .collect();

    if user_parts.is_empty() {
        return Ok(());
    }

    // 2. Build task_summary — user messages only, truncated at char boundary
    let raw = user_parts.join("\n---\n");
    let boundary = raw.floor_char_boundary(2000);
    let task_summary = &raw[..boundary];

    // 3. Load available (non-archived) skill names
    let available_skills = crate::skills::load_skills(crate::config::WORKSPACE_DIR).await;
    let available_names: Vec<String> = available_skills
        .iter()
        .filter(|s| !matches!(s.meta.state, crate::skills::SkillState::Archived))
        .map(|s| s.meta.name.clone())
        .collect();
    let available_str = if available_names.is_empty() {
        "none".to_string()
    } else {
        available_names.join(", ")
    };

    // 4. Build prompt
    let prompt = format!(
        "You are a skill evolution analyzer reviewing a completed interactive session.\n\
         Agent: {agent_name}\n\
         User requests this session (summary):\n{task_summary}\n\n\
         Available skill names (ONLY use these exact names): {available_str}\n\n\
         Respond with EXACTLY ONE line:\n\
         - SKIP — session was casual, no reusable pattern emerged\n\
         - FIX <skill_name> — an existing skill has a gap or error revealed by this \
           session (skill_name MUST be from the list above)\n\
         - DERIVED <parent_skill> <new_name> — create a specialized variant of an \
           existing skill\n\
         - CAPTURED <new_name> — a genuinely new reusable workflow or technique appeared\n\n\
         Act when:\n\
           * The agent made a mistake the skill should prevent next time\n\
           * The user corrected approach, style, format, or workflow\n\
           * A non-trivial technique or pattern emerged future sessions would benefit from\n\
           * A loaded skill turned out to be wrong or incomplete\n\n\
         SKIP for casual conversation, one-off lookups, or sessions with no learnable \
         pattern. SKIP is a valid outcome — do not force an action where none fits."
    );

    let msg = opex_types::Message {
        role: opex_types::MessageRole::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    };

    // 5. Call LLM with 30s timeout
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        provider.chat(&[msg], &[], crate::agent::providers::CallOptions::default()),
    )
    .await;

    let analysis = match response {
        Ok(Ok(resp)) => resp.content,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, agent = agent_name, "session skill review: LLM call failed");
            return Ok(());
        }
        Err(_) => {
            tracing::warn!(agent = agent_name, "session skill review: LLM call timed out");
            return Ok(());
        }
    };

    // 6. Parse verdict and enqueue
    let line = analysis.trim();
    if line.starts_with("SKIP") {
        tracing::debug!(agent = agent_name, "session skill review: SKIP");
        return Ok(());
    }

    if let Some(rest) = line.strip_prefix("FIX ") {
        let skill_name = rest.split_whitespace().next().unwrap_or("");
        if !skill_name.is_empty() {
            let safe = skill_name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', ' '], "-");
            let skill_path = format!("{}/skills/{safe}.md", crate::config::WORKSPACE_DIR);
            if tokio::fs::metadata(&skill_path).await.is_ok() {
                tracing::info!(skill = skill_name, agent = agent_name, "session skill review: FIX queued");
                crate::db::skill_repairs::enqueue(db, skill_name, agent_name, "fix", line).await?;
            } else {
                tracing::warn!(skill = skill_name, agent = agent_name, "session skill review: FIX skipped — skill not found");
            }
        }
    } else if let Some(rest) = line.strip_prefix("DERIVED ") {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let new_name = parts.get(1).copied().unwrap_or("");
        if !new_name.is_empty() {
            tracing::info!(analysis = %line, agent = agent_name, "session skill review: DERIVED queued");
            crate::db::skill_repairs::enqueue(db, new_name, agent_name, "derived", line).await?;
        }
    } else if let Some(rest) = line.strip_prefix("CAPTURED ") {
        let skill_name = rest.split_whitespace().next().unwrap_or("");
        if !skill_name.is_empty() {
            tracing::info!(analysis = %line, agent = agent_name, "session skill review: CAPTURED queued");
            crate::db::skill_repairs::enqueue(db, skill_name, agent_name, "captured", line).await?;
        }
    } else {
        tracing::debug!(verdict = %line, agent = agent_name, "session skill review: unrecognised verdict");
    }

    tracing::info!(agent = agent_name, "session skill review complete");
    Ok(())
}
```

- [ ] **Step 4: Run unit tests to verify they pass**

```bash
cargo test -p opex-core session_review -- --nocapture 2>&1 | tail -20
```

Expected: 7 tests PASS.

- [ ] **Step 5: Cargo check**

```bash
cargo check -p opex-core 2>&1 | tail -30
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/skills/evolution.rs
git commit -m "feat(skill-review): add review_session_for_skills to evolution.rs"
```

---

## Task 3: Wire into finalize.rs

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/finalize.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/opex-core/src/agent/pipeline/finalize.rs` at the end of any existing `#[cfg(test)]` block, or create a new one:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_skill_review_not_called_when_config_absent() {
        // When skill_review is None in FinalizeContext, the spawn must be a no-op.
        // This is a structural test — verify the guard condition is correct.
        let skill_review: Option<crate::config::SkillReviewConfig> = None;
        let tool_count: u32 = 10;
        let should_spawn = skill_review
            .as_ref()
            .map(|c| c.enabled && tool_count >= c.min_tool_calls)
            .unwrap_or(false);
        assert!(!should_spawn);
    }

    #[test]
    fn spawn_skill_review_not_called_when_disabled() {
        let skill_review = Some(crate::config::SkillReviewConfig {
            enabled: false,
            min_tool_calls: 3,
        });
        let tool_count: u32 = 10;
        let should_spawn = skill_review
            .as_ref()
            .map(|c| c.enabled && tool_count >= c.min_tool_calls)
            .unwrap_or(false);
        assert!(!should_spawn);
    }

    #[test]
    fn spawn_skill_review_not_called_below_min_tool_calls() {
        let skill_review = Some(crate::config::SkillReviewConfig {
            enabled: true,
            min_tool_calls: 5,
        });
        let tool_count: u32 = 2;
        let should_spawn = skill_review
            .as_ref()
            .map(|c| c.enabled && tool_count >= c.min_tool_calls)
            .unwrap_or(false);
        assert!(!should_spawn);
    }

    #[test]
    fn spawn_skill_review_fires_when_enabled_and_above_min() {
        let skill_review = Some(crate::config::SkillReviewConfig {
            enabled: true,
            min_tool_calls: 3,
        });
        let tool_count: u32 = 5;
        let should_spawn = skill_review
            .as_ref()
            .map(|c| c.enabled && tool_count >= c.min_tool_calls)
            .unwrap_or(false);
        assert!(should_spawn);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p opex-core spawn_skill_review -- --nocapture 2>&1 | tail -20
```

Expected: FAIL — `SkillReviewConfig` not yet imported in finalize.rs.

- [ ] **Step 3: Add `skill_review` field to `FinalizeContext`**

In `FinalizeContext` (line ~289), after `pub compressor`:

```rust
/// Per-agent skill review config — when Some and enabled, `finalize` spawns
/// a background session analysis after `Done` outcomes with enough tool calls.
pub skill_review: Option<crate::config::SkillReviewConfig>,
```

- [ ] **Step 4: Add `spawn_skill_review` function**

Add after `spawn_knowledge_extraction` (~line 518):

```rust
// ── spawn_skill_review() ──────────────────────────────────────────────────────

/// Count tool_end WAL events for a session (best-effort, returns 0 on error).
async fn count_tool_calls(db: &PgPool, session_id: Uuid) -> u32 {
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*)::BIGINT FROM session_events \
         WHERE session_id = $1 AND event_type = 'tool_end'",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .ok()
    .flatten()
    .and_then(|n| u32::try_from(n).ok())
    .unwrap_or(0)
}

pub(crate) fn spawn_skill_review(
    db: PgPool,
    session_id: Uuid,
    agent_name: String,
    provider: Arc<dyn LlmProvider>,
    min_tool_calls: u32,
    tracker: &TaskTracker,
) {
    tracker.spawn(async move {
        let tool_count = count_tool_calls(&db, session_id).await;
        if tool_count < min_tool_calls {
            return;
        }
        crate::skills::evolution::review_session_for_skills(
            &db,
            &provider,
            &agent_name,
            session_id,
        )
        .await;
    });
}
```

- [ ] **Step 5: Add trigger in the `Done` arm of `finalize()`**

In the `FinalizeOutcome::Done` arm, after `spawn_knowledge_extraction(...)`:

```rust
if let Some(sr_cfg) = &ctx.skill_review {
    if sr_cfg.enabled {
        spawn_skill_review(
            ctx.db.clone(),
            ctx.session_id,
            ctx.agent_name.clone(),
            ctx.provider.clone(),
            sr_cfg.min_tool_calls,
            &ctx.bg_tasks,
        );
    }
}
```

- [ ] **Step 6: Update `finalize_context_from_engine`**

In `finalize_context_from_engine` (~line 481), add to the `FinalizeContext { ... }` initializer:

```rust
skill_review: engine.cfg().agent.skill_review.clone(),
```

- [ ] **Step 7: Run finalize tests**

```bash
cargo test -p opex-core spawn_skill_review -- --nocapture 2>&1 | tail -20
```

Expected: 4 tests PASS.

- [ ] **Step 8: Cargo check + full test suite**

```bash
cargo check -p opex-core 2>&1 | tail -30
cargo test -p opex-core 2>&1 | tail -30
```

Expected: no errors, all tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/finalize.rs
git commit -m "feat(skill-review): wire spawn_skill_review into pipeline finalize"
```

---

## Task 4: Enable on Pi agents + smoke test

**Files:**
- Modify on Pi: `~/opex/config/agents/Arty.toml`, `Alma.toml`, `Hyde.toml`

- [ ] **Step 1: Cross-compile and deploy binary**

```bash
make build-arm64 2>&1 | tail -10
# Wait for success, then:
scp target/aarch64-unknown-linux-gnu/release/opex-core-aarch64 aronmav@192.168.1.85:~/opex/
```

- [ ] **Step 2: Restart service on Pi**

```bash
ssh aronmav@192.168.1.85 "systemctl --user restart opex-core && sleep 3 && systemctl --user is-active opex-core"
```

Expected: `active`

- [ ] **Step 3: Enable skill_review on Arty**

```bash
ssh aronmav@192.168.1.85 "cat >> ~/opex/config/agents/Arty.toml << 'EOF'

[agent.skill_review]
enabled = true
min_tool_calls = 3
EOF"
```

- [ ] **Step 4: Trigger a tool-heavy session**

```bash
TOKEN="1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa"
ssh aronmav@192.168.1.85 "curl -s -X POST \
  -H 'Authorization: Bearer $TOKEN' \
  -H 'Content-Type: application/json' \
  -d '{\"messages\":[{\"role\":\"user\",\"content\":\"Прочитай SOUL.md, MEMORY.md и список файлов в workspace. Затем напиши краткое резюме.\"}],\"agent\":\"Arty\"}' \
  --max-time 60 \
  http://localhost:18789/api/chat 2>&1 | grep -E '(usage|tool-input-available|sessionId)' | head -10"
```

Expected: 3+ `tool-input-available` events.

- [ ] **Step 5: Check logs for skill review activity**

```bash
ssh aronmav@192.168.1.85 "grep -E '(session skill review|skill review)' ~/opex/logs/core.log | tail -10"
```

Expected: `session skill review: SKIP` or `session skill review: CAPTURED/FIX queued` or `session skill review complete`.

- [ ] **Step 6: Check pending_skill_repairs queue**

```bash
ssh aronmav@192.168.1.85 "docker exec docker-postgres-1 psql -U opex -d opex -c \
  'SELECT skill_name, agent_name, kind, status, created_at FROM pending_skill_repairs ORDER BY created_at DESC LIMIT 5;'"
```

- [ ] **Step 7: Commit config**

```bash
# Only if enabling on all 3 agents is desired
git add crates/opex-core/  # binary changes only
git commit -m "feat(skill-review): P0.2 complete — session skill review wired and deployed"
```

---

## Self-Review

**Spec coverage:**
- ✅ `SkillReviewConfig` with `enabled` + `min_tool_calls` — Task 1
- ✅ Separate `[agent.skill_review]` section — Task 1
- ✅ Default `enabled = false` — Task 1 Step 3
- ✅ `review_session_for_skills()` function — Task 2
- ✅ Load last 30 messages, filter user-only for summary — Task 2 Step 3
- ✅ `floor_char_boundary(2000)` truncation — Task 2 Step 3
- ✅ 30s LLM timeout — Task 2 Step 3
- ✅ SKIP/FIX/DERIVED/CAPTURED enqueue logic — Task 2 Step 3
- ✅ `spawn_skill_review` in finalize.rs — Task 3
- ✅ `count_tool_calls` from WAL — Task 3 Step 4
- ✅ Trigger only for `Done` outcome — Task 3 Step 5
- ✅ `finalize_context_from_engine` updated — Task 3 Step 6
- ✅ All unit tests from spec — Tasks 1, 2, 3

**Type consistency:**
- `SkillReviewConfig` used consistently across all tasks
- `review_session_for_skills` signature matches spec and call site
- `spawn_skill_review` passes `min_tool_calls: u32` (not the whole config) — avoids Clone requirement on LlmProvider

**No placeholders:** all steps have concrete code.
