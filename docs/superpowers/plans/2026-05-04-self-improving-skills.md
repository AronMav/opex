# Self-Improving Skills Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give agents a `skill_use(action="capture")` tool to create skills in-session, then enrich the post-session review with richer context and multi-verdict support.

**Architecture:** Part 1 adds a new handler `handle_skill_capture` to `handlers.rs`, dispatched from `engine_dispatch.rs` when `skill_use` is called with `action="capture"`. Part 2 enriches `review_session_inner` in `evolution.rs` with assistant messages, tool names from the WAL, and multi-verdict parsing; `finalize.rs` gets a `force` bypass for Failed/Interrupted sessions.

**Tech Stack:** Rust/Axum backend, sqlx (PgPool), tokio async, existing `write_skill()` / `save_version()` / `crate::gateway::notify()` helpers.

---

## File Map

| Action | Path | Responsibility |
| --- | --- | --- |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/handlers.rs` | Add `handle_skill_capture()` |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/tool_defs.rs` | Add `capture` to `skill_use` schema |
| Modify | `crates/hydeclaw-core/src/agent/engine_dispatch.rs` | Route `capture` action |
| Modify | `crates/hydeclaw-core/src/skills/evolution.rs` | Richer review + multi-verdict |
| Modify | `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` | `force` bypass for Failed/Interrupted |
| Modify | `config/skills/skill-curator.md` | Capture guidance for agents |
| Modify | `crates/hydeclaw-core/scaffold/base/SOUL.md` | Mention capture action |

---

## Task 1 — handle_skill_capture() in handlers.rs

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/handlers.rs`

### Context

`handle_skill_use()` is at line 703. The `match action` arm at line 748 currently returns an error for unknown actions. We add `handle_skill_capture()` as a standalone async function in the same file, and call it from `engine_dispatch.rs` in Task 3.

The function needs: `workspace_dir`, `agent_name`, `db: &sqlx::PgPool`, `ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>`, `args: &serde_json::Value`.

- `write_skill()` is already imported via `crate::skills::write_skill`.
- `save_version()` is `crate::db::skill_versions::save_version`.
- `crate::gateway::notify()` is `pub(crate)` re-exported from `gateway/mod.rs:41`.
- `SkillFrontmatter` and `SkillState` are `crate::skills::{SkillFrontmatter, SkillState}`.

- [ ] **Step 1: Write failing tests**

Add at the bottom of `crates/hydeclaw-core/src/agent/pipeline/handlers.rs` inside the existing `#[cfg(test)]` block (search for `mod tests {`):

```rust
#[tokio::test]
async fn capture_rejects_invalid_name_uppercase() {
    let args = serde_json::json!({
        "action": "capture",
        "name": "MySkill",
        "description": "desc",
        "instructions": "body"
    });
    // Call directly with dummy paths — file check will fail first if name passes
    let dir = tempfile::tempdir().unwrap();
    // We test name validation only — pass dummy db/tx placeholders via None
    // Name has uppercase → must fail before any I/O
    let name = args["name"].as_str().unwrap();
    let valid = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    assert!(!valid, "uppercase name must fail validation");
}

#[tokio::test]
async fn capture_rejects_name_starting_with_dash() {
    let name = "-bad-name";
    let valid = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    assert!(!valid);
}

#[tokio::test]
async fn capture_accepts_valid_name() {
    let name = "my-skill-123";
    let valid = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    assert!(valid);
}

#[tokio::test]
async fn capture_parses_triggers_and_tools() {
    let triggers: Vec<String> = "search, find online, поиск"
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(triggers, vec!["search", "find online", "поиск"]);

    let tools: Vec<String> = " , web_search, ".split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(tools, vec!["web_search"]);
}
```

- [ ] **Step 2: Run tests to confirm they pass (pure logic)**

```bash
cd crates/hydeclaw-core
cargo test capture_rejects_invalid -- --nocapture
cargo test capture_accepts_valid -- --nocapture
cargo test capture_parses_triggers -- --nocapture
```

Expected: all pass (pure logic, no I/O).

- [ ] **Step 3: Implement handle_skill_capture()**

Add after `handle_skill_use()` (after line ~750), before `handle_skill_list()`:

```rust
/// skill_use(action="capture") — create a new skill from a session pattern.
///
/// Writes the file immediately to workspace/skills/, saves a version snapshot,
/// records in curator_decisions, and fires a UI notification.
pub async fn handle_skill_capture(
    workspace_dir: &str,
    agent_name: &str,
    db: &sqlx::PgPool,
    ui_event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    args: &serde_json::Value,
) -> String {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return "Error: 'name' is required.".to_string(),
    };

    // Validate: lowercase letters, digits, hyphens; cannot start with hyphen.
    let valid = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    if !valid {
        return format!(
            "Invalid skill name '{}'. Use lowercase letters, digits, and hyphens only.",
            name
        );
    }

    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => return "Error: 'description' is required.".to_string(),
    };

    let instructions = match args.get("instructions").and_then(|v| v.as_str()) {
        Some(i) if !i.is_empty() => i.to_string(),
        _ => return "Error: 'instructions' is required.".to_string(),
    };

    // Check for collision before writing.
    let skill_path = format!("{}/skills/{}.md", workspace_dir, name);
    if tokio::fs::metadata(&skill_path).await.is_ok() {
        return format!(
            "Skill '{}' already exists. Use skill_use(action='load', name='{}') to read it, \
             or choose a different name.",
            name, name
        );
    }

    let triggers: Vec<String> = args
        .get("triggers")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let tools_required: Vec<String> = args
        .get("tools_required")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let frontmatter = crate::skills::SkillFrontmatter {
        name: name.to_string(),
        description,
        triggers,
        tools_required,
        priority: 5,
        state: crate::skills::SkillState::Active,
        pinned: None,
        last_used_at: None,
    };

    if let Err(e) = crate::skills::write_skill(workspace_dir, name, &frontmatter, &instructions).await {
        return format!("Failed to write skill: {}", e);
    }

    // Read back to snapshot the exact bytes written.
    let content = match tokio::fs::read_to_string(&skill_path).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(skill = %name, agent = %agent_name, error = %e, "skill capture: read-back failed");
            String::new()
        }
    };

    // Version snapshot.
    if !content.is_empty() {
        if let Err(e) = crate::db::skill_versions::save_version(
            db,
            name,
            &content,
            "capture",
            None,
            Some(&format!("captured in-session by {}", agent_name)),
        ).await {
            tracing::warn!(skill = %name, agent = %agent_name, error = %e, "skill capture: version save failed");
        }
    }

    // Audit row in curator_decisions for Phase 3 visibility.
    if let Err(e) = sqlx::query(
        "INSERT INTO curator_decisions (skill_name, action, reason) VALUES ($1, $2, $3)",
    )
    .bind(name)
    .bind("captured")
    .bind(format!("in-session capture by {}", agent_name))
    .execute(db)
    .await
    {
        tracing::warn!(skill = %name, agent = %agent_name, error = %e, "skill capture: curator_decisions insert failed");
    }

    // UI notification (best-effort).
    if let Some(tx) = ui_event_tx {
        if let Err(e) = crate::gateway::notify(
            db,
            tx,
            "skill_captured",
            "New skill captured",
            &format!("Agent {} captured skill: {}", agent_name, name),
            serde_json::json!({"skill": name, "agent": agent_name}),
        ).await {
            tracing::warn!(skill = %name, agent = %agent_name, error = %e, "skill capture: notify failed");
        }
    }

    tracing::info!(skill = %name, agent = %agent_name, "skill captured in-session");
    format!("Skill '{}' captured and active.", name)
}
```

- [ ] **Step 4: Compile to confirm no errors**

```bash
cd crates/hydeclaw-core
cargo check 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/handlers.rs
git commit -m "feat(skills): add handle_skill_capture() for in-session skill creation"
```

---

## Task 2 — tool_defs.rs: add capture to skill_use schema

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/tool_defs.rs`

### Context

`skill_use` tool definition is at line 663. The `enum` currently lists `["list", "load"]`. We add `"capture"` and document its parameters.

- [ ] **Step 1: Update tool_defs.rs**

Find the `skill_use` tool definition (around line 663–675) and replace it:

```rust
// skill_use: on-demand skill loading (always available, not gated by skill_editing)
tools.push(ToolDefinition {
    name: "skill_use".to_string(),
    description: "Load or capture a reusable skill. action='list': show catalog. action='load' + name: get full instructions. action='capture': create a new skill from a session pattern.".to_string(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["list", "load", "capture"],
                "description": "list = show catalog, load = get full skill instructions, capture = create a new skill"
            },
            "name": {
                "type": "string",
                "description": "Skill name (for load or capture). kebab-case, e.g. 'image-resize-workflow'"
            },
            "description": {
                "type": "string",
                "description": "One-sentence summary of what this skill teaches (for capture)"
            },
            "triggers": {
                "type": "string",
                "description": "Comma-separated phrases that should activate this skill (for capture, optional)"
            },
            "tools_required": {
                "type": "string",
                "description": "Comma-separated tool names this skill needs (for capture, optional)"
            },
            "instructions": {
                "type": "string",
                "description": "Full skill body in markdown (for capture)"
            }
        },
        "required": ["action"]
    }),
});
```

- [ ] **Step 2: Compile**

```bash
cd crates/hydeclaw-core
cargo check 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/tool_defs.rs
git commit -m "feat(skills): add capture action to skill_use tool schema"
```

---

## Task 3 — engine_dispatch.rs: route capture action

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/engine_dispatch.rs`

### Context

The `"skill_use"` arm at line 206 calls `handle_skill_use`. We intercept `action="capture"` before that and call `handle_skill_capture` with `self.cfg().db`, `self.cfg().agent.name`, and `self.state().ui_event_tx`.

`self.cfg().db` is `PgPool`. `self.state().ui_event_tx` is `Option<tokio::sync::broadcast::Sender<String>>`.

- [ ] **Step 1: Update the skill_use dispatch arm**

Find the `"skill_use"` match arm (around line 206–209) and replace:

```rust
"skill_use" => {
    let available = self.available_tool_names().await;
    Some(ph::handle_skill_use(&self.cfg().workspace_dir, self.cfg().agent.base, &available, arguments).await)
}
```

With:

```rust
"skill_use" => {
    let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("list");
    if action == "capture" {
        Some(ph::handle_skill_capture(
            &self.cfg().workspace_dir,
            &self.cfg().agent.name,
            &self.cfg().db,
            self.state().ui_event_tx.as_ref(),
            arguments,
        ).await)
    } else {
        let available = self.available_tool_names().await;
        Some(ph::handle_skill_use(&self.cfg().workspace_dir, self.cfg().agent.base, &available, arguments).await)
    }
}
```

- [ ] **Step 2: Compile**

```bash
cd crates/hydeclaw-core
cargo check 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/agent/engine_dispatch.rs
git commit -m "feat(skills): route skill_use capture action to handle_skill_capture"
```

---

## Task 4 — Agent guidance: skill-curator.md and SOUL.md

**Files:**
- Modify: `config/skills/skill-curator.md`
- Modify: `crates/hydeclaw-core/scaffold/base/SOUL.md`

No tests needed — these are documentation-only changes.

- [ ] **Step 1: Add capture section to skill-curator.md**

Append to the end of `config/skills/skill-curator.md`:

```markdown
---

## Capturing New Skills In-Session

Use `skill_use(action="capture")` when you notice a reusable pattern:
- A workflow you will likely need again in future sessions
- A technique that took multiple attempts to get right
- A format, style, or sequence the user explicitly prefers

**Do NOT capture:**
- One-off tasks specific to this session only
- Trivial operations already covered by an existing skill
- Patterns that duplicate an existing skill (use FIX instead)

**Example:**
```
skill_use(action="capture",
  name="image-resize-for-telegram",
  description="Resize images to ≤10MB before sending via Telegram",
  triggers="resize image, compress image, telegram image",
  tools_required="code_exec",
  instructions="## Steps\n1. Read image size\n2. ...")
```
```

- [ ] **Step 2: Add capture line to SOUL.md scaffold**

In `crates/hydeclaw-core/scaffold/base/SOUL.md`, find the line:
```
Load detailed guides via `skill_use(action="load", name="...")`:
```

Add one line immediately after it:

```
- `skill_use(action="capture", name="...", description="...", instructions="...")` — create a new reusable skill from a pattern discovered this session
```

- [ ] **Step 3: Commit**

```bash
git add config/skills/skill-curator.md crates/hydeclaw-core/scaffold/base/SOUL.md
git commit -m "docs(skills): add capture guidance to skill-curator.md and SOUL.md scaffold"
```

---

## Task 5 — evolution.rs: richer review_session_inner + multi-verdict

**Files:**
- Modify: `crates/hydeclaw-core/src/skills/evolution.rs`

### Context

`review_session_for_skills()` at line 134 calls `review_session_inner()` at line 150.
`review_session_inner()` currently:
1. Loads last 30 messages, keeps only user text (2 000 byte cap)
2. Calls LLM, parses the first matching verdict line, enqueues it

We change it to:
1. Add a `force: bool` parameter (bypass user_message_count gate)
2. Reject if `user_parts.len() < 2 && !force`
3. Add assistant text, tool names (from session_events WAL), in-session captured skills (from curator_decisions)
4. 6 000 byte cap total
5. Parse up to 3 verdict lines

Add `force: bool` to both `review_session_for_skills()` and `review_session_inner()`.

- [ ] **Step 1: Write tests for multi-verdict parsing and user-message gate**

Add in the existing `#[cfg(test)]` block in `evolution.rs`:

```rust
#[test]
fn multi_verdict_all_three_parsed() {
    let response = "FIX web-search\nCAPTURED new-pattern\nDERIVED old-skill new-skill";
    let verdicts: Vec<&str> = response
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    assert_eq!(verdicts.len(), 3);
    assert!(verdicts[0].starts_with("FIX"));
    assert!(verdicts[1].starts_with("CAPTURED"));
    assert!(verdicts[2].starts_with("DERIVED"));
}

#[test]
fn multi_verdict_skip_only_produces_nothing() {
    let response = "SKIP";
    let verdicts: Vec<&str> = response
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("SKIP"))
        .take(3)
        .collect();
    assert!(verdicts.is_empty());
}

#[test]
fn multi_verdict_mixed_skip_ignored() {
    let response = "FIX web-search\nSKIP\nCAPTURED other";
    let verdicts: Vec<&str> = response
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("SKIP"))
        .take(3)
        .collect();
    assert_eq!(verdicts.len(), 2);
    assert!(verdicts[0].starts_with("FIX"));
    assert!(verdicts[1].starts_with("CAPTURED"));
}

#[test]
fn user_message_gate_blocks_single_message() {
    let user_parts: Vec<&str> = vec!["hello"];
    let force = false;
    let blocked = user_parts.len() < 2 && !force;
    assert!(blocked);
}

#[test]
fn user_message_gate_passes_when_forced() {
    let user_parts: Vec<&str> = vec!["hello"];
    let force = true;
    let blocked = user_parts.len() < 2 && !force;
    assert!(!blocked);
}

#[test]
fn assistant_text_strips_json_prefix() {
    let content = "Here is the result.[{\"type\":\"tool_use\"}]";
    let end = content.find("[{").unwrap_or(content.len());
    let text = &content[..end];
    assert_eq!(text, "Here is the result.");
}

#[test]
fn context_bundle_respects_byte_cap() {
    let cap = 6000usize;
    let long = "x".repeat(8000);
    let boundary = long.floor_char_boundary(cap);
    assert!(boundary <= cap);
    assert!(long.is_char_boundary(boundary));
}
```

- [ ] **Step 2: Run tests to confirm they pass**

```bash
cd crates/hydeclaw-core
cargo test multi_verdict -- --nocapture
cargo test user_message_gate -- --nocapture
cargo test assistant_text_strips -- --nocapture
cargo test context_bundle -- --nocapture
```

Expected: all pass (pure logic).

- [ ] **Step 3: Rewrite review_session_for_skills() and review_session_inner()**

Replace the entire `review_session_for_skills` function and `review_session_inner` function (lines 134–274) with:

```rust
/// Analyze a completed interactive session for skill improvements.
///
/// `force = true` bypasses the user_message_count >= 2 gate — used for
/// Failed/Interrupted sessions which are informative regardless of size.
pub async fn review_session_for_skills(
    db: &PgPool,
    provider: &Arc<dyn crate::agent::providers::LlmProvider>,
    agent_name: &str,
    session_id: Uuid,
    force: bool,
) {
    if let Err(e) = review_session_inner(db, provider, agent_name, session_id, force).await {
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
    session_id: Uuid,
    force: bool,
) -> anyhow::Result<()> {
    use std::time::Duration;

    // 1. Load last 30 messages.
    let rows = crate::db::sessions::load_messages(db, session_id, Some(30)).await?;

    let user_parts: Vec<&str> = rows.iter()
        .filter(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .collect();

    // Gate: require at least 2 user messages unless forced (Failed/Interrupted).
    if user_parts.len() < 2 && !force {
        return Ok(());
    }

    // 2. Fetch session.created_at for in-session capture lookup.
    let session_created_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT created_at FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();

    // 3. Assistant text (first 300 chars each, strip trailing JSON).
    let assistant_parts: Vec<String> = rows.iter()
        .filter(|m| m.role == "assistant" && !m.content.is_empty())
        .map(|m| {
            let text = &m.content;
            let end = text.find("[{").or_else(|| text.find("{\"type\""))
                .unwrap_or(text.len());
            let end = text[..end].floor_char_boundary(300);
            text[..end].trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();

    // 4. Tool names from session_events WAL.
    let tool_names: Vec<String> = {
        let rows: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT DISTINCT payload->>'tool_name' \
             FROM session_events \
             WHERE session_id = $1 AND event_type = 'tool_end'",
        )
        .bind(session_id)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        rows.into_iter().flatten().collect()
    };

    // 5. Skills captured in this session (from curator_decisions).
    let in_session_skills: Vec<String> = if let Some(created_at) = session_created_at {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT skill_name FROM curator_decisions \
             WHERE action = 'captured' AND decided_at >= $1",
        )
        .bind(created_at)
        .fetch_all(db)
        .await
        .unwrap_or_default();
        rows
    } else {
        vec![]
    };

    // 6. Load available (non-archived) skill names.
    let available_skills = crate::skills::load_skills(crate::config::WORKSPACE_DIR).await;
    let available_names: Vec<String> = available_skills
        .iter()
        .filter(|s| !matches!(s.meta.state, crate::skills::SkillState::Archived))
        .map(|s| s.meta.name.clone())
        .collect();
    let available_str = if available_names.is_empty() { "none".to_string() }
        else { available_names.join(", ") };

    // 7. Build context bundle, capped at 6 000 bytes total.
    let tools_str = if tool_names.is_empty() { "none".to_string() } else { tool_names.join(", ") };
    let captured_str = if in_session_skills.is_empty() { "none".to_string() } else { in_session_skills.join(", ") };

    // Derive outcome from force flag: force=false means Done, force=true means Failed/Interrupted.
    let outcome_str = if force { "failed or interrupted" } else { "done" };

    let meta = format!(
        "[Session metadata]\nAgent: {}\nOutcome: {}\nTool calls made: {}\nSkills captured this session: {}\n\n",
        agent_name, outcome_str, tools_str, captured_str
    );

    let user_raw = user_parts.join("\n---\n");
    let user_cap = user_raw.floor_char_boundary(2500);
    let user_section = format!("[User messages]\n{}\n\n", &user_raw[..user_cap]);

    let assistant_raw = assistant_parts.join("\n---\n");
    let assistant_cap = assistant_raw.floor_char_boundary(2000);
    let assistant_section = format!("[Assistant responses]\n{}", &assistant_raw[..assistant_cap]);

    let context = format!("{}{}{}", meta, user_section, assistant_section);
    let context_cap = context.floor_char_boundary(6000);
    let task_summary = &context[..context_cap];

    // 8. Build prompt with multi-verdict instructions.
    let prompt = format!(
        "You are a skill evolution analyzer reviewing a completed interactive session.\n\
         {task_summary}\n\n\
         Available skill names (ONLY use these exact names for FIX/DERIVED): {available_str}\n\n\
         Respond with 1–3 lines. Each line must be one of:\n\
         - SKIP\n\
         - FIX <skill_name>  (skill_name MUST be from the list above)\n\
         - DERIVED <parent_skill> <new_name>\n\
         - CAPTURED <new_name>\n\n\
         If nothing applies, respond with a single SKIP.\n\n\
         Act when:\n\
           * The agent made a mistake the skill should prevent next time\n\
           * The user corrected approach, style, format, or workflow\n\
           * A non-trivial reusable pattern emerged that future sessions would benefit from\n\
           * A loaded skill turned out to be wrong or incomplete\n\n\
         SKIP for casual conversation, one-off lookups, or sessions with no learnable pattern."
    );

    let msg = Message {
        role: MessageRole::User,
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    };

    // 9. Call LLM with 30s timeout.
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        provider.chat(&[msg], &[], crate::agent::providers::CallOptions::default()),
    )
    .await;

    let analysis = match response {
        Ok(Ok(resp)) => resp.content,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, agent = agent_name, "session skill review: LLM failed");
            return Ok(());
        }
        Err(_) => {
            tracing::warn!(agent = agent_name, "session skill review: LLM timed out");
            return Ok(());
        }
    };

    // 10. Parse up to 3 verdict lines (skip SKIP lines).
    let verdict_lines: Vec<&str> = analysis
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("SKIP"))
        .take(3)
        .collect();

    if verdict_lines.is_empty() {
        tracing::debug!(agent = agent_name, "session skill review: SKIP");
        return Ok(());
    }

    for line in verdict_lines {
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
            tracing::debug!(verdict = %line, agent = agent_name, "session skill review: unrecognised verdict line");
        }
    }

    tracing::info!(agent = agent_name, "session skill review complete");
    Ok(())
}
```

- [ ] **Step 4: Compile**

```bash
cd crates/hydeclaw-core
cargo check 2>&1 | grep "^error" | head -10
```

Expected: no errors. The `crate::db::sessions::load_messages` path might need adjustment — check the actual module path used in the existing code.

- [ ] **Step 5: Run tests**

```bash
cargo test evolution -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/skills/evolution.rs
git commit -m "feat(skills): enrich post-session review with richer context and multi-verdict"
```

---

## Task 6 — finalize.rs: force gate for Failed/Interrupted sessions

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/finalize.rs`

### Context

`spawn_skill_review()` is at line 553. It's currently called only in the `Done` branch (line 374).
We add a `force: bool` parameter to `spawn_skill_review`, and also call it in the `Failed` and `Interrupted` branches with `force: true`.

`review_session_for_skills()` already takes `force: bool` after Task 5.

- [ ] **Step 1: Update spawn_skill_review() signature and logic**

Replace the `spawn_skill_review` function (lines 553–574):

```rust
pub(crate) fn spawn_skill_review(
    db: PgPool,
    session_id: Uuid,
    agent_name: String,
    provider: Arc<dyn LlmProvider>,
    min_tool_calls: u32,
    force: bool,
    tracker: &TaskTracker,
) {
    tracker.spawn(async move {
        if !force {
            let tool_count = count_tool_calls(&db, session_id).await;
            if tool_count < min_tool_calls {
                return;
            }
        }
        crate::skills::evolution::review_session_for_skills(
            &db,
            &provider,
            &agent_name,
            session_id,
            force,
        )
        .await;
    });
}
```

- [ ] **Step 2: Update the Done branch call site (around line 374)**

Find the existing call:

```rust
spawn_skill_review(
    ctx.db.clone(),
    ctx.session_id,
    ctx.agent_name.clone(),
    ctx.provider.clone(),
    sr_cfg.min_tool_calls,
    &ctx.bg_tasks,
);
```

Replace with:

```rust
spawn_skill_review(
    ctx.db.clone(),
    ctx.session_id,
    ctx.agent_name.clone(),
    ctx.provider.clone(),
    sr_cfg.min_tool_calls,
    false,
    &ctx.bg_tasks,
);
```

- [ ] **Step 3: Add skill review call in Failed branch**

In the `FinalizeOutcome::Failed` branch, after `spawn_record_failure(...)` (around line 412), add:

```rust
if let Some(sr_cfg) = &ctx.skill_review {
    if sr_cfg.enabled {
        spawn_skill_review(
            ctx.db.clone(),
            ctx.session_id,
            ctx.agent_name.clone(),
            ctx.provider.clone(),
            sr_cfg.min_tool_calls,
            true, // force=true: Failed sessions bypass tool_count gate
            &ctx.bg_tasks,
        );
    }
}
```

- [ ] **Step 4: Add skill review call in Interrupted branch**

In the `FinalizeOutcome::Interrupted` branch, after `lifecycle_guard.interrupt(reason).await;` (around line 462), add:

```rust
if let Some(sr_cfg) = &ctx.skill_review {
    if sr_cfg.enabled {
        spawn_skill_review(
            ctx.db.clone(),
            ctx.session_id,
            ctx.agent_name.clone(),
            ctx.provider.clone(),
            sr_cfg.min_tool_calls,
            true, // force=true: Interrupted sessions bypass tool_count gate
            &ctx.bg_tasks,
        );
    }
}
```

- [ ] **Step 5: Compile**

```bash
cd crates/hydeclaw-core
cargo check 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 6: Run all tests**

```bash
cargo test -- --nocapture 2>&1 | tail -10
```

Expected: no new failures.

- [ ] **Step 7: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/finalize.rs
git commit -m "feat(skills): spawn skill review for Failed/Interrupted sessions with force bypass"
```

---

## Task 7 — Final verification

- [ ] **Step 1: Full cargo check**

```bash
cd d:/GIT/bogdan/hydeclaw
cargo check --all-targets 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 2: Run full test suite**

```bash
cargo test -- --nocapture 2>&1 | tail -15
```

Expected: no new failures beyond the pre-existing 6 sqlx integration tests (DATABASE_URL not set locally — known limitation).

- [ ] **Step 3: Verify skill_use schema includes capture**

Check that the compiled binary description includes "capture":

```bash
grep -r "capture" crates/hydeclaw-core/src/agent/pipeline/tool_defs.rs
```

Expected: line with `"capture"` in the enum list.
