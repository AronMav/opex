# Memory Extraction Cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop saving individual `auto:session:*` memory chunks — use extracted facts only to update the rolling summary, then discard them; remove the now-dead `save_if_new` machinery; tighten the extraction prompt.

**Architecture:** Single file change in `knowledge_extractor.rs` — remove the persistence loop (lines 147–183) and all code it depended on (`save_if_new`, `save_if_new_with_provider`, `resolve_conflict`, their helpers, tests, and constants). Replace the extraction prompt with stricter rules. Run a one-time SQL cleanup on the database.

**Tech Stack:** Rust 2024 edition, sqlx 0.8, `cargo test --lib`, PostgreSQL (Pi), `docker exec` for DB access.

---

## File Map

| File | Change |
|------|--------|
| `crates/hydeclaw-core/src/agent/knowledge_extractor.rs` | Remove `tool_insights` field; replace extraction prompt; delete persistence loop + dead code + their tests; add one new test |
| DB on Pi (one-time, not a migration) | `DELETE FROM memory_chunks WHERE source LIKE 'auto:session:%'` |

---

## Task 1: Remove `tool_insights` from `ExtractedKnowledge` and fix tests

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/knowledge_extractor.rs`

Context: `ExtractedKnowledge` (lines 25–35) has a `tool_insights: Vec<String>` field. `update_rolling_summary` already skips it (lines 200–203 only iterate user_facts/outcomes/feedback). Eight `parse_extraction` tests reference `result.tool_insights` and will fail to compile once the field is removed.

- [ ] **Step 1: Remove the field**

In `knowledge_extractor.rs`, replace the struct (lines 25–35):

```rust
#[derive(Debug, Deserialize)]
struct ExtractedKnowledge {
    #[serde(default)]
    user_facts: Vec<String>,
    #[serde(default)]
    outcomes: Vec<String>,
    #[serde(default)]
    feedback: Vec<String>,
}
```

- [ ] **Step 2: Verify compile fails at the expected spots**

```bash
cargo check -p hydeclaw-core 2>&1 | grep "error\[" | head -20
```

Expected errors at:
- Lines 520, 543, 552, 561, 590, 631 — `result.tool_insights` in `parse_extraction` tests
- Lines 652–653 — `tool_insights: vec![...]` in `rolling_summary_collects_only_user_facts_outcomes_feedback`
- Lines 853–854 — `tool_insights: vec![...]` in `rolling_summary_empty_when_only_tool_insights`

- [ ] **Step 3: Fix all `tool_insights` compile errors in the test block**

**3a. Replace six `parse_extraction` tests** (remove `tool_insights` from JSON inputs and assertions):

```rust
#[test]
fn parse_clean_json() {
    let input = r#"{"user_facts":["User works in IT"],"outcomes":["Decided to use GraphQL"],"feedback":[]}"#;
    let result = parse_extraction(input).unwrap();
    assert_eq!(result.user_facts, vec!["User works in IT"]);
    assert_eq!(result.outcomes, vec!["Decided to use GraphQL"]);
}

#[test]
fn parse_with_surrounding_text() {
    let input = "Based on my analysis:\n\n{\"user_facts\":[\"A\"],\"outcomes\":[\"B\"],\"feedback\":[]}\n\nI hope this helps!";
    let result = parse_extraction(input).unwrap();
    assert_eq!(result.user_facts, vec!["A"]);
    assert_eq!(result.outcomes, vec!["B"]);
}

#[test]
fn parse_empty_arrays() {
    let input = r#"{"user_facts":[],"outcomes":[],"feedback":[]}"#;
    let result = parse_extraction(input).unwrap();
    assert!(result.user_facts.is_empty());
    assert!(result.outcomes.is_empty());
    assert!(result.feedback.is_empty());
}

#[test]
fn parse_missing_fields_default_empty() {
    let input = r#"{"user_facts":["Only this"]}"#;
    let result = parse_extraction(input).unwrap();
    assert_eq!(result.user_facts, vec!["Only this"]);
    assert!(result.outcomes.is_empty());
    assert!(result.feedback.is_empty());
}

#[test]
fn parse_multiple_items_per_category() {
    let input = r#"{"user_facts":["F1","F2","F3"],"outcomes":["O1","O2"],"feedback":["FB1"]}"#;
    let result = parse_extraction(input).unwrap();
    assert_eq!(result.user_facts.len(), 3);
    assert_eq!(result.outcomes.len(), 2);
    assert_eq!(result.feedback.len(), 1);
}

#[test]
fn parse_with_feedback_field() {
    let input = r#"{"user_facts":["F1"],"outcomes":["O1"],"feedback":["User approved the analysis","User rejected the recommendation"]}"#;
    let result = parse_extraction(input).unwrap();
    assert_eq!(result.feedback.len(), 2);
    assert_eq!(result.feedback[0], "User approved the analysis");
}
```

**3b. Rewrite `rolling_summary_collects_only_user_facts_outcomes_feedback` (lines 647–665):**

Replace the whole test — it constructs `ExtractedKnowledge` with `tool_insights` which no longer exists:

```rust
#[test]
fn rolling_summary_collects_from_all_three_categories() {
    let extracted = ExtractedKnowledge {
        user_facts: vec!["User works in IT".into()],
        outcomes: vec!["Decided to use GraphQL".into()],
        feedback: vec!["User approved analysis".into()],
    };
    let mut facts: Vec<&str> = Vec::new();
    for f in &extracted.user_facts { facts.push(f); }
    for f in &extracted.outcomes { facts.push(f); }
    for f in &extracted.feedback { facts.push(f); }
    assert_eq!(facts.len(), 3);
    assert!(facts.iter().any(|f| f.contains("IT")));
    assert!(facts.iter().any(|f| f.contains("GraphQL")));
    assert!(facts.iter().any(|f| f.contains("approved")));
}
```

**3c. Delete `rolling_summary_empty_when_only_tool_insights` (lines 849–861):**

Delete the entire test — it tests behavior that no longer exists (tool_insights being excluded):

```rust
// DELETE this test:
#[test]
fn rolling_summary_empty_when_only_tool_insights() { ... }
```

- [ ] **Step 4: Run tests — expect all to pass**

```bash
cargo test -p hydeclaw-core --lib 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok. N passed` (no failures).

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/knowledge_extractor.rs
git commit -m "refactor(memory): remove tool_insights from ExtractedKnowledge"
```

---

## Task 2: Replace extraction prompt and update module docstring

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/knowledge_extractor.rs`

Context: Extraction prompt (lines 103–125) is the source of noise — it allows meta-commentary, session actions, and up to 5 items per category. The module docstring (line 3) mentions "tool insights".

- [ ] **Step 1: Update the module docstring (line 3)**

```rust
//! Post-session knowledge extraction.
//!
//! After a session completes with ≥ 5 messages, extracts user facts, outcomes,
//! and feedback via LLM and uses them to update the rolling summary in memory.
```

- [ ] **Step 2: Replace the extraction prompt (lines 103–125)**

Replace the entire `let prompt = format!(...)` block:

```rust
    // 4. Call LLM for extraction
    let prompt = format!(
        "You are a knowledge extraction assistant. Analyze the conversation below and extract information worth remembering long-term.\n\n\
         Return a JSON object with three arrays:\n\
         {{\n\
           \"user_facts\": [\"...\"],\n\
           \"outcomes\": [\"...\"],\n\
           \"feedback\": [\"...\"]\n\
         }}\n\n\
         Categories:\n\
         - user_facts: Stable facts about the user — preferences, domain knowledge, long-term goals, identity\n\
         - outcomes: Durable decisions, agreements, or corrections that affect future sessions\n\
         - feedback: User's explicit reactions — what they approved, rejected, asked to redo\n\n\
         Rules (STRICTLY enforce):\n\
         - TIMELESS TEST: would this fact still matter in 6 months? If no — skip it.\n\
         - DO NOT extract what happened in this session: actions taken, requests made, things fixed/deleted/deployed.\n\
         - DO NOT extract facts implied by the conversation topic itself.\n\
         - Each item must be self-contained and make sense without reading the session.\n\
         - Write in the same language as the conversation.\n\
         - Maximum 3 items per category.\n\
         - Return empty arrays if nothing passes the timeless test.\n\n\
         Conversation:\n{}", conversation
    );
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p hydeclaw-core 2>&1 | grep "^error"
```

Expected: no output.

- [ ] **Step 4: Run tests — expect all to pass**

```bash
cargo test -p hydeclaw-core --lib 2>&1 | grep -E "test result|FAILED"
```

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/knowledge_extractor.rs
git commit -m "fix(memory): tighten extraction prompt — timeless test, no session actions, max 3"
```

---

## Task 3: Remove persistence loop and all dead code

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/knowledge_extractor.rs`

Context: Lines 147–183 save each fact to `memory_chunks`. After removing them, `save_if_new`, `save_if_new_with_provider`, `resolve_conflict`, `ConflictDecision`, `parse_conflict_decision`, `DEDUP_THRESHOLD`, and `CONFLICT_THRESHOLD` become dead code with no callers outside tests. Their tests (`save_if_new_*` block, lines 593–625) also disappear.

- [ ] **Step 1: Delete the individual persistence block (lines 147–183)**

Delete from `// 6. Dedup and save each fact` through the closing `}` of the `if saved > 0` block, leaving only:

```rust
    // 5. Parse JSON from response
    let extracted = parse_extraction(&response.content)?;

    // 6. Update rolling agent summary
    update_rolling_summary(agent_name, provider, memory_store, &extracted).await;

    Ok(())
```

- [ ] **Step 2: Delete `DEDUP_THRESHOLD` constant (line 23)**

```rust
// DELETE this line:
const DEDUP_THRESHOLD: f64 = 0.9;
```

- [ ] **Step 3: Delete dead functions and helpers (lines 331–506)**

Delete the entire block from `/// Similarity thresholds for conflict resolution.` through the closing `}` of `parse_conflict_decision`. This covers:
- `CONFLICT_THRESHOLD`
- `save_if_new`
- `save_if_new_with_provider`
- `resolve_conflict`
- `ConflictDecision`
- `parse_conflict_decision`

- [ ] **Step 4: Delete all test blocks that call deleted functions**

There are three groups to delete:

**4a. `parse_conflict_*` tests (lines 667–796)** — call `parse_conflict_decision` which is deleted:

Delete from `// ── conflict resolution tests` (line 667) through `parse_conflict_empty_json` closing `}` (line 796). This covers 11 tests: `parse_conflict_update`, `parse_conflict_add`, `parse_conflict_noop`, `parse_conflict_delete`, `parse_conflict_with_think_blocks`, `parse_conflict_malformed_defaults_to_add`, `parse_conflict_with_markdown_fences`, `parse_conflict_missing_target_defaults_to_zero`, `parse_conflict_missing_reason`, `parse_conflict_unknown_action_preserved`, `parse_conflict_empty_json`.

**4b. `save_if_new_*` tests (lines 593–625 and 798–827)** — call `save_if_new` which is deleted:

Delete from `// ── save_if_new tests` (line 593) through `save_if_new_accepts_shared_scope` (line 625). Then also delete from `// ── save_if_new threshold tests` (line 798) through `save_if_new_unavailable_store_returns_false` (line 827). This removes 8 `#[tokio::test]` functions total.

**4c. `extraction_scope_assignment` test (lines 829–846)** — documents the old 4-field design:

Delete from `// ── scope consistency tests` (line 829) through the closing `}` of `extraction_scope_assignment` (line 846).

- [ ] **Step 5: Add the schema regression test**

Add at the end of the `#[cfg(test)]` block (before the final `}`):

```rust
    #[test]
    fn extracted_knowledge_has_no_tool_insights_field() {
        // Compile-time guarantee: serde roundtrip succeeds without tool_insights.
        // If the field is ever re-added, assertions in parse_* tests will catch it.
        let json = r#"{"user_facts":["x"],"outcomes":[],"feedback":[]}"#;
        let parsed: ExtractedKnowledge = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.user_facts, vec!["x"]);
        assert!(parsed.outcomes.is_empty());
        assert!(parsed.feedback.is_empty());
    }
```

- [ ] **Step 6: Verify compile + run tests**

```bash
cargo check --all-targets 2>&1 | grep "^error"
cargo test --lib 2>&1 | grep -E "test result|FAILED"
```

Expected: no errors, all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/hydeclaw-core/src/agent/knowledge_extractor.rs
git commit -m "refactor(memory): remove individual fact persistence and dead code"
```

---

## Task 4: One-time DB cleanup on Pi

**Context:** Existing `auto:session:*` rows in `memory_chunks` will never be queried again — new sessions no longer create them and rolling summary already absorbed any useful content. Clean them up before deploy.

- [ ] **Step 1: Count existing rows (before)**

```bash
ssh aronmav@192.168.1.85 "docker exec \$(docker ps -q --filter name=postgres) \
  psql -U hydeclaw -d hydeclaw \
  -c \"SELECT COUNT(*) FROM memory_chunks WHERE source LIKE 'auto:session:%';\""
```

Note the count.

- [ ] **Step 2: Run the DELETE**

```bash
ssh aronmav@192.168.1.85 "docker exec \$(docker ps -q --filter name=postgres) \
  psql -U hydeclaw -d hydeclaw \
  -c \"DELETE FROM memory_chunks WHERE source LIKE 'auto:session:%';\""
```

Expected output: `DELETE N` where N matches Step 1 count.

- [ ] **Step 3: Verify zero rows remain**

```bash
ssh aronmav@192.168.1.85 "docker exec \$(docker ps -q --filter name=postgres) \
  psql -U hydeclaw -d hydeclaw \
  -c \"SELECT COUNT(*) FROM memory_chunks WHERE source LIKE 'auto:session:%';\""
```

Expected: `count = 0`.

---

## Task 5: Final verification and deploy

- [ ] **Step 1: Full cargo check**

```bash
cargo check --all-targets 2>&1 | grep "^error"
```

Expected: no output.

- [ ] **Step 2: Full unit test run**

```bash
cargo test --lib 2>&1 | grep -E "test result|FAILED"
```

Expected: all crates show `test result: ok`.

- [ ] **Step 3: Push**

```bash
git push origin master
```

- [ ] **Step 4: Build ARM64 and deploy binary to Pi**

```bash
cargo zigbuild --release --target aarch64-unknown-linux-gnu -p hydeclaw-core
ssh aronmav@192.168.1.85 "systemctl --user stop hydeclaw-core"
scp target/aarch64-unknown-linux-gnu/release/hydeclaw-core aronmav@192.168.1.85:~/hydeclaw/hydeclaw-core-aarch64
ssh aronmav@192.168.1.85 "systemctl --user start hydeclaw-core && sleep 3 && systemctl --user is-active hydeclaw-core"
```

Expected: `active`

- [ ] **Step 5: Verify no new `auto:session:*` entries after a complete session**

Start a session on the Pi (via UI or API), let it complete, then:

```bash
ssh aronmav@192.168.1.85 "docker exec \$(docker ps -q --filter name=postgres) \
  psql -U hydeclaw -d hydeclaw \
  -c \"SELECT COUNT(*) FROM memory_chunks WHERE source LIKE 'auto:session:%';\""
```

Expected: `count = 0` (no new entries created).

- [ ] **Step 6: Verify rolling summary was updated**

```bash
ssh aronmav@192.168.1.85 "docker exec \$(docker ps -q --filter name=postgres) \
  psql -U hydeclaw -d hydeclaw \
  -c \"SELECT source, LEFT(content, 100) FROM memory_chunks WHERE source LIKE 'rolling_summary:%';\""
```

Expected: one row per agent with non-empty content.
