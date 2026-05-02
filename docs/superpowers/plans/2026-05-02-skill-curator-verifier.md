# Skill Curator Phase 3 — Analyst/Verifier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split Phase 3 skill consolidation into three independent steps: Analyst (Hyde session writes proposals.json), Verifier (1-turn session per ARCHIVE checks capability coverage), Executor (applies only verified changes).

**Architecture:** Analyst writes structured JSON to `workspace/curator_proposals.json` via workspace_write. Core parses JSON, runs a focused 1-turn verifier session per ARCHIVE proposal (max_iterations=1, no tools). Only ACCEPT proposals are applied — ARCHIVEs programmatically, MERGE/FIX via a second Hyde session. Verifier cannot inherit Analyst's reasoning because it only receives the raw capability_map + skill file contents, never the session history.

**Tech Stack:** Rust, `serde_json`, existing `run_agent_task` / `run_subagent`, `phase_transitions` frontmatter update pattern.

---

## File Structure

| File | Change |
|---|---|
| `crates/hydeclaw-core/src/curator/phase_consolidation.rs` | Full rewrite: data types, analyst, verifier, executor, wired `run` |
| `crates/hydeclaw-core/src/curator/mod.rs` | Add `run_verifier_task` helper |

No DB schema changes. No changes to `mod.rs` orchestrator, `scheduler/mod.rs`, `main.rs`, or any handler.

---

## Task 1: Proposal data types

**Files:**
- Modify: `crates/hydeclaw-core/src/curator/phase_consolidation.rs`

- [ ] **Step 1: Replace file contents with data type definitions + unit tests**

```rust
//! Phase 3 — Analyst / Verifier / Executor skill consolidation.

use serde::Deserialize;
use crate::gateway::clusters::AgentCore;

// ── Public result type ────────────────────────────────────────────────────────

pub struct ConsolidationResult {
    pub commands_executed: i32,
    pub log: Vec<String>,
}

// ── Proposal data types ───────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct CapabilityEntry {
    pub capability: String,
    pub from_quote: String,
    pub covered_in: String,
    pub covering_quote: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub(crate) enum Proposal {
    Archive {
        skill: String,
        replacement: String,
        #[allow(dead_code)]
        reason: String,
        capability_map: Vec<CapabilityEntry>,
    },
    Merge {
        sources: Vec<String>,
        into: String,
        reason: String,
    },
    Fix {
        skill: String,
        description: String,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProposalsFile {
    pub proposals: Vec<Proposal>,
}

// ── Entry point stub (filled in Task 6) ──────────────────────────────────────

pub async fn run(
    _workspace_dir: &str,
    _agents: &AgentCore,
    _agent_name: &str,
) -> anyhow::Result<ConsolidationResult> {
    Ok(ConsolidationResult { commands_executed: 0, log: vec!["not yet implemented".into()] })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposals_valid_json_parses() {
        let json = r#"{
            "proposals": [
                {
                    "action": "archive",
                    "skill": "daily-reflection",
                    "replacement": "self-improvement",
                    "reason": "covered",
                    "capability_map": [
                        {
                            "capability": "journal format",
                            "from_quote": "# Journal: YYYY-MM-DD",
                            "covered_in": "self-improvement Section 1",
                            "covering_quote": "# Journal: YYYY-MM-DD"
                        }
                    ]
                },
                {
                    "action": "fix",
                    "skill": "research-strategy",
                    "description": "add section on source validation"
                }
            ]
        }"#;
        let parsed: ProposalsFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.proposals.len(), 2);
        match &parsed.proposals[0] {
            Proposal::Archive { skill, capability_map, .. } => {
                assert_eq!(skill, "daily-reflection");
                assert_eq!(capability_map.len(), 1);
            }
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn proposals_invalid_json_returns_err() {
        let result: Result<ProposalsFile, _> = serde_json::from_str("not json {{");
        assert!(result.is_err());
    }

    #[test]
    fn proposals_unknown_action_returns_err() {
        let json = r#"{"proposals": [{"action": "delete", "skill": "x"}]}"#;
        let result: Result<ProposalsFile, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p hydeclaw-core curator::phase_consolidation::tests -- --nocapture
```

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/curator/phase_consolidation.rs
git commit -m "feat(curator/p3): add proposal data types and parsing tests"
```

---

## Task 2: `run_verifier_task` helper

**Files:**
- Modify: `crates/hydeclaw-core/src/curator/mod.rs`

- [ ] **Step 1: Write the failing test for pinned exclusion helper**

Add inside `mod tests` in `mod.rs`:

```rust
#[test]
fn pinned_names_not_in_summary() {
    let summary = "- name: web-search\n- name: code-methodology\n- name: memory-management";
    let pinned = vec!["code-methodology".to_string(), "memory-management".to_string()];
    // The analyst task must not expose pinned skill names as archiving candidates.
    // We verify this by checking the pinned list is communicated in the task header.
    let task = format!(
        "NEVER touch pinned skills: [{}]\nSkills:\n{}",
        pinned.join(", "),
        summary
    );
    assert!(task.contains("code-methodology"));
    assert!(task.contains("memory-management"));
    // Pinned names appear in the forbidden list, not just in the summary
    let forbidden_section = task.split("Skills:").next().unwrap();
    assert!(forbidden_section.contains("code-methodology"));
}
```

- [ ] **Step 2: Run test to verify it passes** (it's a pure string test, no DB needed)

```bash
cargo test -p hydeclaw-core curator::tests::pinned_names_not_in_summary -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Add `run_verifier_task` to `mod.rs`**

Add after the existing `run_agent_task` function:

```rust
/// Run a single-turn verification task through a named agent.
/// Uses max_iterations=1 and an empty tool list so the agent cannot call tools —
/// it must respond with plain text (ACCEPT or REJECT).
pub(crate) async fn run_verifier_task(
    agents: &AgentCore,
    agent_name: &str,
    task: &str,
) -> anyhow::Result<String> {
    let engine = agents.get_engine(agent_name).await
        .ok_or_else(|| anyhow::anyhow!("curator: agent not found: {agent_name}"))?;
    engine
        .run_subagent(task, 1, None, None, None, Some(vec![]))
        .await
        .map_err(|e| anyhow::anyhow!("curator: verifier session failed: {e}"))
}
```

- [ ] **Step 4: Verify compilation**

```bash
cargo check -p hydeclaw-core 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/curator/mod.rs
git commit -m "feat(curator/p3): add run_verifier_task helper (1-turn, no tools)"
```

---

## Task 3: Analyst step

**Files:**
- Modify: `crates/hydeclaw-core/src/curator/phase_consolidation.rs`

- [ ] **Step 1: Write tests for `read_proposals`**

Add to the `#[cfg(test)]` block:

```rust
#[tokio::test]
async fn read_proposals_missing_file_returns_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("curator_proposals.json");
    // File does not exist
    let result = read_proposals_file(path.to_str().unwrap()).await;
    assert!(result.proposals.is_empty());
}

#[tokio::test]
async fn read_proposals_invalid_json_returns_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("curator_proposals.json");
    tokio::fs::write(&path, b"{{not json").await.unwrap();
    let result = read_proposals_file(path.to_str().unwrap()).await;
    assert!(result.proposals.is_empty());
}
```

- [ ] **Step 2: Run tests — expect compile failure** (function doesn't exist yet)

```bash
cargo test -p hydeclaw-core curator::phase_consolidation::tests::read_proposals -- --nocapture 2>&1 | head -5
```

Expected: compile error mentioning `read_proposals_file`.

- [ ] **Step 3: Add `read_proposals_file` and `run_analyst`**

Add before the `run` stub in `phase_consolidation.rs`:

```rust
// ── Analyst ───────────────────────────────────────────────────────────────────

/// Read and parse the proposals file written by the Analyst session.
/// Returns an empty ProposalsFile on any error (missing file, invalid JSON).
async fn read_proposals_file(path: &str) -> ProposalsFile {
    match tokio::fs::read_to_string(path).await {
        Err(e) => {
            tracing::warn!(path, error = %e, "curator p3: proposals file not found");
            ProposalsFile { proposals: vec![] }
        }
        Ok(content) => match serde_json::from_str(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(path, error = %e, "curator p3: proposals file invalid JSON");
                ProposalsFile { proposals: vec![] }
            }
        },
    }
}

/// Run the Analyst Hyde session. Hyde reads active skills and writes
/// `workspace/curator_proposals.json` via workspace_write.
async fn run_analyst(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
    active_summary: &str,
    pinned_names: &[String],
) -> anyhow::Result<ProposalsFile> {
    let proposals_path = std::path::Path::new(workspace_dir)
        .join("curator_proposals.json")
        .to_string_lossy()
        .to_string();

    let task = format!(
        "[Curator: skill consolidation — analyst pass]\n\
         Analyse the active skill collection and write your proposals to \
         workspace/curator_proposals.json using workspace_write.\n\n\
         NEVER touch pinned skills: [{}]\n\n\
         MANDATORY for any ARCHIVE proposal:\n\
         1. Read the FULL content of the skill to archive via workspace_read\n\
         2. Read the FULL content of the replacement skill via workspace_read\n\
         3. For every distinct capability/section in the archived skill, include a \
            capability_map entry with verbatim from_quote (from archived) and \
            covering_quote (from replacement).\n\
         4. If any capability has no verbatim covering_quote — do NOT include that \
            ARCHIVE proposal at all.\n\n\
         JSON schema for workspace/curator_proposals.json:\n\
         {{\n\
           \"proposals\": [\n\
             {{ \"action\": \"archive\", \"skill\": \"name\", \"replacement\": \"name\",\n\
                \"reason\": \"one sentence\",\n\
                \"capability_map\": [\n\
                  {{ \"capability\": \"name\", \"from_quote\": \"verbatim\",\n\
                     \"covered_in\": \"section\", \"covering_quote\": \"verbatim\" }}\n\
                ]\n\
             }},\n\
             {{ \"action\": \"merge\", \"sources\": [\"a\",\"b\"], \"into\": \"c\", \
                \"reason\": \"one sentence\" }},\n\
             {{ \"action\": \"fix\", \"skill\": \"name\", \"description\": \"what to fix\" }}\n\
           ]\n\
         }}\n\n\
         Rules:\n\
         - Maximum 5 proposals (ARCHIVE + MERGE + FIX combined)\n\
         - 'Never used' (last_used_at=null) is NOT a reason to archive\n\
         - Write ONLY the JSON file. Do not reply with analysis text.\n\n\
         Active skills (summary — read full files before deciding):\n{}",
        pinned_names.join(", "),
        active_summary
    );

    if let Err(e) = crate::curator::run_agent_task(agents, agent_name, &task).await {
        tracing::warn!(error = %e, "curator p3: analyst session failed");
    }

    Ok(read_proposals_file(&proposals_path).await)
}
```

Also add `tempfile` to dev-dependencies in `Cargo.toml` if not already present:

```bash
grep -n "tempfile" crates/hydeclaw-core/Cargo.toml
```

If absent, add to `[dev-dependencies]`:
```toml
tempfile = "3"
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p hydeclaw-core curator::phase_consolidation::tests::read_proposals -- --nocapture
```

Expected: 2 new tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/curator/phase_consolidation.rs crates/hydeclaw-core/Cargo.toml
git commit -m "feat(curator/p3): add analyst step and proposals file reader"
```

---

## Task 4: Verifier step

**Files:**
- Modify: `crates/hydeclaw-core/src/curator/phase_consolidation.rs`

- [ ] **Step 1: Write tests for `parse_verifier_response`**

Add to `#[cfg(test)]`:

```rust
#[test]
fn verifier_accept_response_is_accepted() {
    assert!(parse_verifier_response("ACCEPT"));
    assert!(parse_verifier_response("ACCEPT\nsome trailing text"));
}

#[test]
fn verifier_reject_response_is_rejected() {
    assert!(!parse_verifier_response("REJECT: journal format — not found in replacement"));
    assert!(!parse_verifier_response("REJECT: cap1 — missing\nREJECT: cap2 — absent"));
}

#[test]
fn verifier_malformed_response_is_rejected() {
    assert!(!parse_verifier_response(""));
    assert!(!parse_verifier_response("I think this looks fine"));
    assert!(!parse_verifier_response("ACCEPTED"));  // typo — not ACCEPT
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p hydeclaw-core curator::phase_consolidation::tests::verifier -- --nocapture 2>&1 | head -5
```

Expected: compile error (function not yet defined).

- [ ] **Step 3: Implement `parse_verifier_response` and `verify_archive_proposal`**

```rust
// ── Verifier ──────────────────────────────────────────────────────────────────

/// Parse the verifier's plain-text response.
/// Returns true only if the first non-empty line is exactly "ACCEPT" (case-sensitive).
/// "ACCEPTED", "ACCEPT something", empty, or any REJECT response all return false.
fn parse_verifier_response(response: &str) -> bool {
    response.trim_start().lines().next().unwrap_or("").trim() == "ACCEPT"
}

/// Run a 1-turn verifier session for an ARCHIVE proposal.
/// Returns (accepted: bool, reject_reason: String).
async fn verify_archive_proposal(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
    skill: &str,
    replacement: &str,
    capability_map: &[CapabilityEntry],
) -> (bool, String) {
    let skills_dir = std::path::Path::new(workspace_dir).join("skills");
    let safe_skill = crate::curator::sanitize_skill_name(skill);
    let safe_replacement = crate::curator::sanitize_skill_name(replacement);

    let archived_content = match tokio::fs::read_to_string(
        skills_dir.join(format!("{safe_skill}.md"))
    ).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(skill, error = %e, "curator p3 verifier: cannot read archived skill");
            return (false, format!("cannot read skill file: {e}"));
        }
    };

    let replacement_content = match tokio::fs::read_to_string(
        skills_dir.join(format!("{safe_replacement}.md"))
    ).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(replacement, error = %e, "curator p3 verifier: cannot read replacement skill");
            return (false, format!("cannot read replacement file: {e}"));
        }
    };

    let map_json = serde_json::to_string_pretty(capability_map)
        .unwrap_or_else(|_| "[]".to_string());

    let task = format!(
        "You are a skill coverage auditor. Verify that a proposed archival is safe.\n\n\
         Skill to archive: {skill}\n\
         Full content:\n{archived_content}\n\n\
         Proposed replacement: {replacement}\n\
         Full content:\n{replacement_content}\n\n\
         Capability map claimed by analyst:\n{map_json}\n\n\
         For each entry in the capability map:\n\
         1. Find the from_quote in the archived skill (exact or near-exact match required)\n\
         2. Find the covering_quote in the replacement skill (exact or near-exact match required)\n\
         3. Confirm the covering_quote addresses the same capability\n\n\
         Return EXACTLY one of:\n\
           ACCEPT\n\
           REJECT: <capability name> — <reason it is not covered>\n\n\
         Multiple REJECT lines are allowed (one per missing capability).\n\
         Be strict. Paraphrase is not coverage. If a quote is absent — REJECT."
    );

    match crate::curator::run_verifier_task(agents, agent_name, &task).await {
        Ok(response) => {
            let accepted = parse_verifier_response(&response);
            let reason = if accepted {
                String::new()
            } else {
                response.lines()
                    .filter(|l| l.starts_with("REJECT"))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            (accepted, reason)
        }
        Err(e) => {
            tracing::warn!(skill, error = %e, "curator p3 verifier: session failed — treating as REJECT");
            (false, format!("verifier error: {e}"))
        }
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p hydeclaw-core curator::phase_consolidation::tests::verifier -- --nocapture
```

Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/curator/phase_consolidation.rs
git commit -m "feat(curator/p3): add verifier step with parse_verifier_response"
```

---

## Task 5: Executor step

**Files:**
- Modify: `crates/hydeclaw-core/src/curator/phase_consolidation.rs`

- [ ] **Step 1: Add `apply_verified_archive`**

```rust
// ── Executor ──────────────────────────────────────────────────────────────────

/// Programmatically archive a skill: save version snapshot, update frontmatter state.
async fn apply_verified_archive(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    skill: &str,
) -> anyhow::Result<()> {
    let safe = crate::curator::sanitize_skill_name(skill);
    let path = std::path::Path::new(workspace_dir).join("skills").join(format!("{safe}.md"));

    let content = tokio::fs::read_to_string(&path).await
        .map_err(|e| anyhow::anyhow!("apply_verified_archive: read {skill}: {e}"))?;

    // Snapshot before modifying
    let _ = crate::db::skill_versions::save_version(
        db, skill, &content, "archive", None,
        Some("curator:archive:verified"),
    ).await;

    // Update state in frontmatter (same pattern as phase_transitions)
    let updated = content.replacen("state: active", "state: archived", 1);
    let updated = if updated == content {
        content.replacen("state: stale", "state: archived", 1)
    } else {
        updated
    };

    let tmp = format!("{}.tmp", path.display());
    tokio::fs::write(&tmp, &updated).await
        .map_err(|e| anyhow::anyhow!("apply_verified_archive: write tmp {skill}: {e}"))?;
    tokio::fs::rename(&tmp, &path).await
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            anyhow::anyhow!("apply_verified_archive: rename {skill}: {e}")
        })?;

    Ok(())
}

/// Run executor Hyde session for MERGE and FIX proposals.
async fn run_executor(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
    proposals: &[&Proposal],
) -> anyhow::Result<i32> {
    if proposals.is_empty() {
        return Ok(0);
    }

    let proposals_text = proposals.iter().enumerate().map(|(i, p)| match p {
        Proposal::Merge { sources, into, reason } => format!(
            "{}. MERGE {} into '{}': {}",
            i + 1, sources.join(" + "), into, reason
        ),
        Proposal::Fix { skill, description } => format!(
            "{}. FIX '{}': {}", i + 1, skill, description
        ),
        _ => String::new(),
    }).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");

    let task = format!(
        "[Curator: skill consolidation — executor pass]\n\
         Apply these approved skill changes using workspace_write / workspace_edit. \
         Workspace skills are in workspace/skills/.\n\n\
         Changes to apply:\n{proposals_text}\n\n\
         For MERGE: create the new merged skill file, then set state: archived in \
         each source skill frontmatter.\n\
         For FIX: edit the skill body in-place."
    );

    crate::curator::run_agent_task(agents, agent_name, &task).await?;
    Ok(proposals.len() as i32)
}

/// Delete the proposals file after processing.
async fn cleanup_proposals(workspace_dir: &str) {
    let path = std::path::Path::new(workspace_dir).join("curator_proposals.json");
    if let Err(e) = tokio::fs::remove_file(&path).await {
        tracing::debug!(error = %e, "curator p3: cleanup proposals file (may not exist)");
    }
}
```

- [ ] **Step 2: Check compilation**

```bash
cargo check -p hydeclaw-core 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/curator/phase_consolidation.rs
git commit -m "feat(curator/p3): add executor (apply_verified_archive + run_executor)"
```

---

## Task 6: Wire up `run`

**Files:**
- Modify: `crates/hydeclaw-core/src/curator/phase_consolidation.rs`

- [ ] **Step 1: Replace the stub `run` with the full implementation**

Replace the stub `run` function (adding `db: &sqlx::PgPool` to the signature):

```rust
pub async fn run(
    workspace_dir: &str,
    agents: &AgentCore,
    agent_name: &str,
    db: &sqlx::PgPool,
) -> anyhow::Result<ConsolidationResult> {
    let mut result = ConsolidationResult { commands_executed: 0, log: Vec::new() };

    // ── Load active skills ────────────────────────────────────────────────────
    let skills = crate::skills::load_skills(workspace_dir).await;
    let active: Vec<_> = skills.iter()
        .filter(|s| !matches!(s.meta.state, crate::skills::SkillState::Archived))
        .collect();

    if active.is_empty() {
        return Ok(ConsolidationResult {
            commands_executed: 0,
            log: vec!["no active skills".into()],
        });
    }

    let pinned: Vec<String> = active.iter()
        .filter(|s| s.meta.pinned.unwrap_or(false))
        .map(|s| s.meta.name.clone())
        .collect();

    let summary = active.iter().map(|s| {
        format!(
            "- name: {}\n  description: {}\n  state: {:?}\n  last_used_at: {}\n  triggers: [{}]",
            s.meta.name,
            s.meta.description,
            s.meta.state,
            s.meta.last_used_at.as_deref().unwrap_or("never"),
            s.meta.triggers.join(", ")
        )
    }).collect::<Vec<_>>().join("\n");

    // ── Step A: Analyst ───────────────────────────────────────────────────────
    let proposals = run_analyst(workspace_dir, agents, agent_name, &summary, &pinned).await?;

    if proposals.proposals.is_empty() {
        result.log.push("Analyst: no proposals.".into());
        cleanup_proposals(workspace_dir).await;
        return Ok(result);
    }

    result.log.push(format!("Analyst: {} proposal(s).", proposals.proposals.len()));

    // ── Step B: Verify ARCHIVE proposals ─────────────────────────────────────
    let mut accepted_archives: Vec<(&str, &str)> = Vec::new(); // (skill, replacement)

    for proposal in &proposals.proposals {
        if let Proposal::Archive { skill, replacement, capability_map, .. } = proposal {
            let (accepted, reason) = verify_archive_proposal(
                workspace_dir, agents, agent_name,
                skill, replacement, capability_map,
            ).await;

            if accepted {
                accepted_archives.push((skill, replacement));
                result.log.push(format!("ARCHIVE `{skill}` → ACCEPTED (verifier)"));
            } else {
                result.log.push(format!("ARCHIVE `{skill}` → REJECTED: {reason}"));
            }
        }
    }

    // ── Step C: Execute ───────────────────────────────────────────────────────

    // Apply accepted ARCHIVEs programmatically
    for (skill, _replacement) in &accepted_archives {
        match apply_verified_archive(workspace_dir, db, skill).await {
            Ok(()) => {
                result.commands_executed += 1;
                tracing::info!(skill, "curator p3: skill archived (verified)");
            }
            Err(e) => {
                tracing::warn!(skill, error = %e, "curator p3: archive apply failed");
                result.log.push(format!("⚠ archive `{skill}` failed: {e}"));
            }
        }
    }

    // Collect accepted MERGE/FIX proposals
    let executor_proposals: Vec<&Proposal> = proposals.proposals.iter()
        .filter(|p| matches!(p, Proposal::Merge { .. } | Proposal::Fix { .. }))
        .collect();

    if !executor_proposals.is_empty() {
        match run_executor(workspace_dir, agents, agent_name, &executor_proposals).await {
            Ok(n) => {
                result.commands_executed += n;
                result.log.push(format!("Executor: applied {n} merge/fix action(s)."));
            }
            Err(e) => {
                tracing::warn!(error = %e, "curator p3: executor session failed");
                result.log.push(format!("⚠ executor failed: {e}"));
            }
        }
    }

    cleanup_proposals(workspace_dir).await;
    Ok(result)
}
```

- [ ] **Step 2: Update `run_curator` in `mod.rs` to pass `db`**

In `crates/hydeclaw-core/src/curator/mod.rs`, in the `run_curator` function, find this line:

```rust
match phase_consolidation::run(workspace_dir, agents.as_ref(), &cfg.agent_name).await {
```

Replace with:

```rust
match phase_consolidation::run(workspace_dir, agents.as_ref(), &cfg.agent_name, db).await {
```

- [ ] **Step 3: Check compilation**

```bash
cargo check -p hydeclaw-core 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 4: Run all curator tests**

```bash
cargo test -p hydeclaw-core curator -- --nocapture
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/curator/phase_consolidation.rs crates/hydeclaw-core/src/curator/mod.rs
git commit -m "feat(curator/p3): wire analyst/verifier/executor into run"
```

---

## Task 7: Build, deploy, regression test

**Files:** none (deploy + verify)

- [ ] **Step 1: Build ARM64**

```bash
cargo zigbuild --target aarch64-unknown-linux-gnu -p hydeclaw-core --release 2>&1 | tail -5
```

Expected: `Finished release profile`.

- [ ] **Step 2: Deploy**

```bash
ssh aronmav@192.168.1.85 "systemctl --user stop hydeclaw-core"
scp target/aarch64-unknown-linux-gnu/release/hydeclaw-core aronmav@192.168.1.85:/home/aronmav/hydeclaw/hydeclaw-core-aarch64
ssh aronmav@192.168.1.85 "systemctl --user start hydeclaw-core && sleep 3 && systemctl --user is-active hydeclaw-core"
```

Expected: `active`.

- [ ] **Step 3: Verify preconditions**

```bash
ssh aronmav@192.168.1.85 "curl -s http://localhost:18789/api/skills \
  -H 'Authorization: Bearer 1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa' \
  | python3 -c \"
import json,sys
skills=json.load(sys.stdin).get('skills',[])
targets=['daily-reflection','verification','news-digest']
for s in skills:
    if s['name'] in targets:
        print(s['name'], s.get('state'), 'pinned=' + str(s.get('pinned',False)))
\""
```

Expected:
```
daily-reflection active pinned=False
verification active pinned=False
news-digest active pinned=True
```

- [ ] **Step 4: Run manual curator and wait for completion**

```bash
RUN_ID=$(ssh aronmav@192.168.1.85 "curl -s -X POST http://localhost:18789/api/curator/run \
  -H 'Authorization: Bearer 1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa'" \
  | grep -o '"run_id":"[^"]*"' | cut -d'"' -f4)
echo "Run: $RUN_ID"

ssh aronmav@192.168.1.85 "until docker run --rm --network host postgres:17 \
  psql postgresql://hydeclaw:hydeclaw@localhost:5432/hydeclaw \
  -tAc \"SELECT status FROM curator_runs WHERE id='$RUN_ID';\" 2>/dev/null \
  | grep -q 'done\|failed\|skipped'; do sleep 8; done; \
  docker run --rm --network host postgres:17 \
  psql postgresql://hydeclaw:hydeclaw@localhost:5432/hydeclaw \
  -tAc \"SELECT status, phase3, report_md FROM curator_runs WHERE id='$RUN_ID';\" 2>/dev/null"
```

- [ ] **Step 5: Verify regression results**

```bash
ssh aronmav@192.168.1.85 "curl -s http://localhost:18789/api/skills \
  -H 'Authorization: Bearer 1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa' \
  | python3 -c \"
import json,sys
skills=json.load(sys.stdin).get('skills',[])
targets=['daily-reflection','verification','news-digest']
for s in skills:
    if s['name'] in targets:
        print(s['name'], s.get('state'))
\""
```

Expected:
```
daily-reflection archived      # correctly archived by verifier
verification archived          # correctly archived by verifier
news-digest active             # pinned — never proposed
```

- [ ] **Step 6: Commit final**

```bash
git add crates/hydeclaw-core/src/curator/
git commit -m "feat(curator/p3): analyst/verifier/executor — regression verified on Pi"
```
