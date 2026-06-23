# P0.1 Trajectory Compression (Hermes Parity) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace reactive overflow-triggered compaction with a proactive, session-stateful Hermes-style compressor that fires before each LLM call, with 5-phase algorithm, anti-thrashing, and iterative summary updates.

**Architecture:** New `agent/compressor.rs` holds per-session state (previous_summary, ineffective_count, last_prompt_tokens). `history.rs` gains `compress_messages()` with 5 phases. `pipeline/execute.rs` checks `should_compress()` before each LLM call using real token counts from prior responses. State persists in `sessions.compaction_state JSONB` across reconnects.

**Tech Stack:** Rust async (tokio), sqlx (Postgres), existing `LlmProvider` trait, `estimate_tokens()` from `history.rs`, `MessageRole` from `opex_types`.

**Spec:** `docs/superpowers/specs/2026-05-02-trajectory-compression-design.md`

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `migrations/040_sessions_compaction_state.sql` | **CREATE** | ADD COLUMN compaction_state JSONB |
| `crates/opex-core/src/db/compaction.rs` | **CREATE** | get/set compaction_state DB queries |
| `crates/opex-core/src/db/mod.rs` | **MODIFY** | pub mod compaction |
| `crates/opex-core/src/agent/compressor.rs` | **CREATE** | Compressor struct + trigger logic |
| `crates/opex-core/src/agent/mod.rs` | **MODIFY** | pub mod compressor |
| `crates/opex-core/src/config/mod.rs` | **MODIFY** | 5 new CompactionConfig fields |
| `crates/opex-core/src/agent/history.rs` | **MODIFY** | 5-phase compress_messages() + helpers |
| `crates/opex-core/src/agent/pipeline/bootstrap.rs` | **MODIFY** | Load compaction_state into BootstrapOutcome |
| `crates/opex-core/src/agent/pipeline/execute.rs` | **MODIFY** | Proactive trigger + update_token_count |
| `crates/opex-core/src/agent/pipeline/finalize.rs` | **MODIFY** | Save compaction_state to DB |

---

## Task 1: Migration + DB helpers

**Files:**
- Create: `migrations/040_sessions_compaction_state.sql`
- Create: `crates/opex-core/src/db/compaction.rs`
- Modify: `crates/opex-core/src/db/mod.rs`

- [ ] **Step 1.1: Write migration**

```sql
-- migrations/040_sessions_compaction_state.sql
ALTER TABLE sessions
  ADD COLUMN IF NOT EXISTS compaction_state JSONB;

COMMENT ON COLUMN sessions.compaction_state IS
  'Compressor per-session state: {previous_summary, ineffective_count, compression_count}. NULL = no compaction yet.';
```

- [ ] **Step 1.2: Write DB helpers**

Create `crates/opex-core/src/db/compaction.rs`:

```rust
use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn get_compaction_state(db: &PgPool, session_id: Uuid) -> Result<Option<serde_json::Value>> {
    let row = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT compaction_state FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.flatten())
}

pub async fn set_compaction_state(
    db: &PgPool,
    session_id: Uuid,
    state: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "UPDATE sessions SET compaction_state = $1 WHERE id = $2",
    )
    .bind(state)
    .bind(session_id)
    .execute(db)
    .await?;
    Ok(())
}
```

- [ ] **Step 1.3: Export from db/mod.rs**

Open `crates/opex-core/src/db/mod.rs` and add:

```rust
pub mod compaction;
```

- [ ] **Step 1.4: Verify migration compiles**

```bash
cd crates/opex-core && cargo check 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 1.5: Commit**

```bash
git add migrations/040_sessions_compaction_state.sql \
        crates/opex-core/src/db/compaction.rs \
        crates/opex-core/src/db/mod.rs
git commit -m "feat(compaction): add sessions.compaction_state column + DB helpers"
```

---

## Task 2: CompactionConfig — 5 new fields

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs`

- [ ] **Step 2.1: Find CompactionConfig in config/mod.rs**

```bash
grep -n "CompactionConfig\|pub struct Compaction" crates/opex-core/src/config/mod.rs
```

Note the line numbers. The struct currently has `enabled`, `threshold`, `preserve_tool_calls`, `preserve_last_n`, `max_context_tokens`.

- [ ] **Step 2.2: Add `#[derive(Default)]` to CompactionConfig**

Find `pub struct CompactionConfig` and check if it already has `#[derive(Default)]`.
If it does, skip this step. If not, add it:

```rust
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct CompactionConfig {
```

Run `cargo check -p opex-core 2>&1 | grep "^error"` — expected: no errors.

- [ ] **Step 2.3: Write failing test**

In `crates/opex-core/src/config/mod.rs` inside `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn compaction_config_new_fields_have_defaults() {
    let cfg: CompactionConfig = toml::from_str("enabled = true").unwrap();
    assert_eq!(cfg.protect_first_n, 3);
    assert!((cfg.summary_target_ratio - 0.20).abs() < 0.001);
    assert!((cfg.anti_thrash_min_savings - 0.10).abs() < 0.001);
    assert_eq!(cfg.anti_thrash_max_skips, 2);
    assert!(cfg.extract_to_memory);
}
```

- [ ] **Step 2.4: Run to confirm it fails**

```bash
cargo test -p opex-core compaction_config_new_fields_have_defaults 2>&1 | tail -5
```

Expected: compile error (fields don't exist yet).

- [ ] **Step 2.5: Add fields to CompactionConfig**

Find the `pub struct CompactionConfig` block and add the 5 new fields with `#[serde(default = "...")]`:

```rust
/// Head protection: number of messages to always keep (system + first user + first assistant).
#[serde(default = "CompactionConfig::default_protect_first_n")]
pub protect_first_n: usize,

/// Tail token budget as a fraction of threshold_tokens.
/// tail_budget = (context_limit * threshold * summary_target_ratio) tokens.
#[serde(default = "CompactionConfig::default_summary_target_ratio")]
pub summary_target_ratio: f64,

/// Skip compression if savings < this fraction. Anti-thrashing.
#[serde(default = "CompactionConfig::default_anti_thrash_min_savings")]
pub anti_thrash_min_savings: f64,

/// Stop attempting compression after this many consecutive ineffective compressions.
#[serde(default = "CompactionConfig::default_anti_thrash_max_skips")]
pub anti_thrash_max_skips: u8,

/// Keep OPEX's pgvector fact extraction alongside the Hermes summary.
#[serde(default = "CompactionConfig::default_extract_to_memory")]
pub extract_to_memory: bool,
```

- [ ] **Step 2.6: Add default fns to the impl block**

In the `impl CompactionConfig` block (or create one), add:

```rust
fn default_protect_first_n() -> usize { 3 }
fn default_summary_target_ratio() -> f64 { 0.20 }
fn default_anti_thrash_min_savings() -> f64 { 0.10 }
fn default_anti_thrash_max_skips() -> u8 { 2 }
fn default_extract_to_memory() -> bool { true }
```

- [ ] **Step 2.7: Run test to confirm it passes**

```bash
cargo test -p opex-core compaction_config_new_fields_have_defaults 2>&1 | tail -5
```

Expected: `test ... ok`.

- [ ] **Step 2.8: Commit**

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(compaction): add 5 new CompactionConfig fields with defaults"
```

---

## Task 3: Compressor struct

**Files:**
- Create: `crates/opex-core/src/agent/compressor.rs`
- Modify: `crates/opex-core/src/agent/mod.rs`

- [ ] **Step 3.1: Write failing tests first**

Create `crates/opex-core/src/agent/compressor.rs` with tests only (no implementation):

```rust
use crate::config::CompactionConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressorState {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub compression_count: u32,
}

pub struct Compressor {
    pub previous_summary: Option<String>,
    pub ineffective_count: u8,
    pub last_prompt_tokens: u32,
    pub compression_count: u32,
    pub context_limit: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(threshold: f64) -> CompactionConfig {
        CompactionConfig {
            enabled: true,
            threshold,
            anti_thrash_min_savings: 0.10,
            anti_thrash_max_skips: 2,
            ..Default::default()
        }
    }

    #[test]
    fn should_compress_false_when_no_prior_response() {
        let c = Compressor::new(128_000);
        assert!(!c.should_compress(&cfg(0.75)));
    }

    #[test]
    fn should_compress_false_below_threshold() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 50_000; // 128000 * 0.75 = 96000 → below
        assert!(!c.should_compress(&cfg(0.75)));
    }

    #[test]
    fn should_compress_true_above_threshold() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 100_000; // above 96000
        assert!(c.should_compress(&cfg(0.75)));
    }

    #[test]
    fn anti_thrash_skips_after_n_ineffective() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 100_000;
        let cfg = cfg(0.75);
        // Record 2 ineffective compressions
        c.record_compression_result(100_000, 98_000, &cfg); // saved 2% < 10%
        c.record_compression_result(98_000, 96_500, &cfg);  // saved 1.5% < 10%
        assert_eq!(c.ineffective_count, 2);
        assert!(!c.should_compress(&cfg));
    }

    #[test]
    fn anti_thrash_resets_on_effective_compression() {
        let mut c = Compressor::new(128_000);
        c.last_prompt_tokens = 100_000;
        let cfg = cfg(0.75);
        c.record_compression_result(100_000, 98_000, &cfg); // ineffective
        c.record_compression_result(100_000, 60_000, &cfg); // saved 40% → reset
        assert_eq!(c.ineffective_count, 0);
        assert!(c.should_compress(&cfg));
    }

    #[test]
    fn load_from_none_gives_fresh_compressor() {
        let c = Compressor::load(None, 64_000);
        assert_eq!(c.context_limit, 64_000);
        assert_eq!(c.ineffective_count, 0);
        assert!(c.previous_summary.is_none());
    }

    #[test]
    fn roundtrip_state_through_json() {
        let mut c = Compressor::new(128_000);
        c.previous_summary = Some("summary text".into());
        c.ineffective_count = 1;
        c.compression_count = 3;
        let json = c.to_json();
        let c2 = Compressor::load(Some(json), 128_000);
        assert_eq!(c2.previous_summary.as_deref(), Some("summary text"));
        assert_eq!(c2.ineffective_count, 1);
        assert_eq!(c2.compression_count, 3);
    }
}
```

- [ ] **Step 3.2: Run to confirm tests fail**

```bash
cargo test -p opex-core compressor 2>&1 | tail -10
```

Expected: compile errors (methods not defined).

- [ ] **Step 3.3: Implement Compressor**

Add the implementation above the `#[cfg(test)]` block:

```rust
impl Compressor {
    pub fn new(context_limit: u32) -> Self {
        Self {
            previous_summary: None,
            ineffective_count: 0,
            last_prompt_tokens: 0,
            compression_count: 0,
            context_limit,
        }
    }

    pub fn load(state: Option<serde_json::Value>, context_limit: u32) -> Self {
        let mut c = Self::new(context_limit);
        if let Some(val) = state {
            match serde_json::from_value::<CompressorState>(val) {
                Ok(s) => {
                    c.previous_summary = s.previous_summary;
                    c.ineffective_count = s.ineffective_count;
                    c.compression_count = s.compression_count;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to deserialize compaction_state, starting fresh");
                }
            }
        }
        c
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(CompressorState {
            previous_summary: self.previous_summary.clone(),
            ineffective_count: self.ineffective_count,
            compression_count: self.compression_count,
        })
        .unwrap_or(serde_json::Value::Null)
    }

    pub fn should_compress(&self, cfg: &CompactionConfig) -> bool {
        if self.last_prompt_tokens == 0 {
            return false;
        }
        let threshold = (self.context_limit as f64 * cfg.threshold) as u32;
        if self.last_prompt_tokens < threshold {
            return false;
        }
        if self.ineffective_count >= cfg.anti_thrash_max_skips {
            tracing::warn!(
                count = self.ineffective_count,
                "compression skipped — last {} compressions each saved <{:.0}% tokens; consider /new",
                self.ineffective_count,
                cfg.anti_thrash_min_savings * 100.0,
            );
            return false;
        }
        true
    }

    pub fn update_token_count(&mut self, input_tokens: u32) {
        self.last_prompt_tokens = input_tokens;
    }

    pub fn record_compression_result(
        &mut self,
        tokens_before: u32,
        tokens_after: u32,
        cfg: &CompactionConfig,
    ) {
        let savings_pct = if tokens_before > 0 {
            (tokens_before.saturating_sub(tokens_after)) as f64 / tokens_before as f64
        } else {
            0.0
        };
        if savings_pct < cfg.anti_thrash_min_savings {
            self.ineffective_count = self.ineffective_count.saturating_add(1);
        } else {
            self.ineffective_count = 0;
        }
        self.compression_count += 1;
        tracing::info!(
            savings_pct = format!("{:.1}%", savings_pct * 100.0),
            compression_count = self.compression_count,
            ineffective_count = self.ineffective_count,
            "compression recorded"
        );
    }
}
```

- [ ] **Step 3.4: Export from agent/mod.rs**

Find `crates/opex-core/src/agent/mod.rs` and add:

```rust
pub mod compressor;
```

- [ ] **Step 3.5: Run tests**

```bash
cargo test -p opex-core compressor 2>&1 | tail -10
```

Expected: all 6 tests pass.

- [ ] **Step 3.6: Commit**

```bash
git add crates/opex-core/src/agent/compressor.rs \
        crates/opex-core/src/agent/mod.rs
git commit -m "feat(compaction): add Compressor struct with should_compress and anti-thrashing"
```

---

## Task 4: Phase 1 — Pre-pass helpers

**Files:**
- Modify: `crates/opex-core/src/agent/history.rs`

These are pure functions (no I/O, no LLM). Add them near the top of `history.rs`.

- [ ] **Step 4.1: Write failing tests**

At the bottom of `history.rs` in the `#[cfg(test)]` block, add:

```rust
#[test]
fn prune_deduplicates_identical_tool_results() {
    use opex_types::MessageRole;
    let dup_content = "x".repeat(300); // > 200 chars threshold
    let msgs = vec![
        Message { role: MessageRole::Tool, content: dup_content.clone(),
                  tool_call_id: Some("a".into()), tool_calls: None, thinking_blocks: vec![] },
        Message { role: MessageRole::Tool, content: dup_content.clone(),
                  tool_call_id: Some("b".into()), tool_calls: None, thinking_blocks: vec![] },
    ];
    let pruned = prune_old_tool_results(&msgs, 0); // protect_tail = 0 (prune all)
    // Older dup (index 0) should be replaced; newer (index 1) kept
    assert!(pruned[0].content.contains("Duplicate"));
    assert_eq!(pruned[1].content, dup_content);
}

#[test]
fn prune_replaces_large_tool_result_with_summary_line() {
    use opex_types::MessageRole;
    let msgs = vec![
        Message { role: MessageRole::Tool, content: "a".repeat(300),
                  tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![] },
    ];
    let pruned = prune_old_tool_results(&msgs, 0);
    // Content replaced with 1-line summary (contains "[" marker)
    assert!(pruned[0].content.starts_with('['));
    assert!(pruned[0].content.len() < 300);
}

#[test]
fn prune_skips_messages_in_protected_tail() {
    use opex_types::MessageRole;
    let content = "b".repeat(300);
    let msgs = vec![
        Message { role: MessageRole::Tool, content: content.clone(),
                  tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![] },
    ];
    let pruned = prune_old_tool_results(&msgs, 1); // protect last 1
    assert_eq!(pruned[0].content, content); // unchanged
}
```

- [ ] **Step 4.2: Run to confirm tests fail**

```bash
cargo test -p opex-core prune_ 2>&1 | tail -5
```

Expected: compile error (`prune_old_tool_results` not found).

- [ ] **Step 4.3: Implement `prune_old_tool_results`**

Add to `history.rs` (before the existing `compact_if_needed`):

```rust
/// Phase 1 pre-pass: replace old tool result contents with 1-line summaries,
/// deduplicate identical results. `protect_tail` is the number of messages from
/// the end that are never pruned (matches `preserve_last_n` from config).
pub fn prune_old_tool_results(messages: &[Message], protect_tail: usize) -> Vec<Message> {
    if messages.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<Message> = messages.to_vec();
    let prune_end = result.len().saturating_sub(protect_tail);

    // Pass 1: deduplicate — keep newest full copy, replace older dups
    use std::collections::HashMap;
    let mut content_hashes: HashMap<u64, usize> = HashMap::new(); // hash → newest index
    for i in (0..result.len()).rev() {
        if result[i].role != MessageRole::Tool { continue; }
        let content = &result[i].content;
        if content.len() < 200 { continue; }
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut hasher);
        let h = hasher.finish();
        if let Some(&newer_idx) = content_hashes.get(&h) {
            if i < newer_idx && i < prune_end {
                result[i].content =
                    "[Duplicate tool output — same content as a more recent call]".into();
            }
        } else {
            content_hashes.insert(h, i);
        }
    }

    // Pass 2: replace large tool results outside protected tail with 1-line summary
    for i in 0..prune_end {
        let msg = &result[i];
        if msg.role != MessageRole::Tool { continue; }
        if msg.content.len() <= 200 { continue; }
        if msg.content.starts_with('[') { continue; } // already pruned/deduped
        let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
        let char_count = msg.content.len();
        result[i].content = format!("[tool result {tool_call_id}] ({char_count} chars — pruned)");
    }

    result
}
```

- [ ] **Step 4.4: Run tests**

```bash
cargo test -p opex-core prune_ 2>&1 | tail -5
```

Expected: all 3 tests pass.

- [ ] **Step 4.5: Commit**

```bash
git add crates/opex-core/src/agent/history.rs
git commit -m "feat(compaction): add Phase 1 pre-pass: prune_old_tool_results"
```

---

## Task 5: Phase 2 — Boundary calculation

**Files:**
- Modify: `crates/opex-core/src/agent/history.rs`

- [ ] **Step 5.1: Write failing tests**

```rust
#[test]
fn tail_cut_respects_token_budget() {
    use opex_types::MessageRole;
    // 10 messages × ~100 tokens each = 1000 tokens total.
    // budget = 200 → tail should be last ~2 messages.
    let msgs: Vec<Message> = (0..10).map(|i| Message {
        role: if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant },
        content: "a".repeat(400), // ~100 tokens each
        tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
    }).collect();
    let tail_start = find_tail_start_by_tokens(&msgs, 0, 200);
    assert!(tail_start >= msgs.len() - 4); // last 2-4 messages in tail
    assert!(tail_start < msgs.len()); // not everything in tail
}

#[test]
fn tail_cut_always_includes_last_user_message() {
    use opex_types::MessageRole;
    let mut msgs: Vec<Message> = (0..8).map(|i| Message {
        role: MessageRole::Assistant,
        content: "a".repeat(400),
        tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
    }).collect();
    // Last user message at index 3
    msgs[3].role = MessageRole::User;
    // Very small budget → tail would normally be last 3 msgs, missing index 3
    let tail_start = find_tail_start_by_tokens(&msgs, 0, 50);
    assert!(tail_start <= 3, "last user message must be in tail, tail_start={tail_start}");
}

#[test]
fn head_end_skips_orphan_tool_results() {
    use opex_types::MessageRole;
    let msgs = vec![
        Message { role: MessageRole::System,    content: "s".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
        Message { role: MessageRole::User,      content: "u".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
        Message { role: MessageRole::Tool,      content: "t".into(), tool_call_id: Some("x".into()), tool_calls: None, thinking_blocks: vec![] },
        Message { role: MessageRole::Assistant, content: "a".into(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
    ];
    // protect_first_n=2 → head_end starts at 2, but index 2 is Tool → slide to 3
    let head_end = find_head_end(&msgs, 2);
    assert_eq!(head_end, 3);
}
```

- [ ] **Step 5.2: Run to confirm they fail**

```bash
cargo test -p opex-core "tail_cut\|head_end" 2>&1 | tail -5
```

Expected: compile errors.

- [ ] **Step 5.3: Implement `find_head_end` and `find_tail_start_by_tokens`**

```rust
/// Phase 2: find where the compressed middle begins.
/// Starts at `protect_first_n`, then slides forward past any leading Tool messages
/// so we never start the summarised region mid-tool-call/result group.
pub fn find_head_end(messages: &[Message], protect_first_n: usize) -> usize {
    let mut idx = protect_first_n.min(messages.len());
    while idx < messages.len() && messages[idx].role == MessageRole::Tool {
        idx += 1;
    }
    idx
}

/// Phase 2: find where the protected tail begins, using a token budget.
/// Walks backward accumulating estimated tokens until `tail_budget` is exhausted.
/// Invariants:
///   - Always keeps at least 3 messages.
///   - The most recent User-role message is always in the tail (active task).
///   - Never returns an index ≤ `head_end`.
pub fn find_tail_start_by_tokens(messages: &[Message], head_end: usize, tail_budget: usize) -> usize {
    let n = messages.len();
    if n <= head_end + 1 {
        return n; // nothing to compress
    }
    let min_tail = 3.min(n.saturating_sub(head_end).saturating_sub(1));
    let soft_ceiling = (tail_budget as f64 * 1.5) as usize;
    let mut accumulated: usize = 0;
    let mut cut_idx = n;

    for i in (head_end..n).rev() {
        let msg_tokens = messages[i].content.len() / 4 + 10;
        if accumulated + msg_tokens > soft_ceiling && n - i >= min_tail {
            break;
        }
        accumulated += msg_tokens;
        cut_idx = i;
    }

    // Hard minimum: last min_tail messages always protected
    cut_idx = cut_idx.min(n.saturating_sub(min_tail));

    // Invariant: most recent User message must be in the tail
    let last_user_idx = messages[head_end..n]
        .iter()
        .rposition(|m| m.role == MessageRole::User)
        .map(|rel| rel + head_end);
    if let Some(user_idx) = last_user_idx {
        if user_idx < cut_idx {
            cut_idx = user_idx; // pull tail start back to include the user message
        }
    }

    // Align backward: if cut lands inside a tool_call/result group, include the whole group
    while cut_idx > head_end && messages[cut_idx].role == MessageRole::Tool {
        cut_idx = cut_idx.saturating_sub(1);
    }
    if cut_idx > head_end
        && messages[cut_idx].role == MessageRole::Assistant
        && messages[cut_idx].tool_calls.is_some()
    {
        cut_idx = cut_idx.saturating_sub(1);
    }

    cut_idx.max(head_end + 1)
}
```

- [ ] **Step 5.4: Run tests**

```bash
cargo test -p opex-core "tail_cut\|head_end" 2>&1 | tail -5
```

Expected: all 3 tests pass.

- [ ] **Step 5.5: Commit**

```bash
git add crates/opex-core/src/agent/history.rs
git commit -m "feat(compaction): add Phase 2 boundary helpers: find_head_end, find_tail_start_by_tokens"
```

---

## Task 6: Phase 5 — Tool pair sanitization

**Files:**
- Modify: `crates/opex-core/src/agent/history.rs`

- [ ] **Step 6.1: Write failing tests**

```rust
#[test]
fn sanitize_removes_orphaned_tool_results() {
    use opex_types::MessageRole;
    let msgs = vec![
        // tool result whose call_id has no matching assistant tool_call
        Message { role: MessageRole::Tool, content: "orphan".into(),
                  tool_call_id: Some("orphan_id".into()), tool_calls: None, thinking_blocks: vec![] },
        Message { role: MessageRole::User, content: "hello".into(),
                  tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
    ];
    let sanitized = sanitize_tool_pairs(msgs);
    assert_eq!(sanitized.len(), 1);
    assert_eq!(sanitized[0].role, MessageRole::User);
}

#[test]
fn sanitize_adds_stub_for_orphaned_calls() {
    use opex_types::{MessageRole, ToolCall};
    let msgs = vec![
        Message {
            role: MessageRole::Assistant,
            content: "".into(),
            tool_calls: Some(vec![ToolCall {
                id: "tc_1".into(),
                name: "workspace_read".into(),
                arguments: serde_json::json!({}),
            }]),
            tool_call_id: None,
            thinking_blocks: vec![],
        },
        // No tool result for tc_1
        Message { role: MessageRole::User, content: "next".into(),
                  tool_calls: None, tool_call_id: None, thinking_blocks: vec![] },
    ];
    let sanitized = sanitize_tool_pairs(msgs);
    // A stub tool result should be inserted after the assistant message
    assert_eq!(sanitized.len(), 3);
    assert_eq!(sanitized[1].role, MessageRole::Tool);
    assert_eq!(sanitized[1].tool_call_id.as_deref(), Some("tc_1"));
    assert!(sanitized[1].content.contains("earlier conversation"));
}
```

- [ ] **Step 6.2: Run to confirm they fail**

```bash
cargo test -p opex-core sanitize_ 2>&1 | tail -5
```

Expected: compile errors.

- [ ] **Step 6.3: Implement `sanitize_tool_pairs`**

```rust
/// Phase 5: fix orphaned tool_call / tool_result pairs after compression.
/// 1. Remove tool results whose tool_call_id has no matching assistant tool_call.
/// 2. Insert stub results for assistant tool_calls that have no result.
pub fn sanitize_tool_pairs(messages: Vec<Message>) -> Vec<Message> {
    use std::collections::HashSet;

    // Collect all call_ids referenced by assistant messages
    let surviving_call_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .flat_map(|m| m.tool_calls.iter().flatten())
        .map(|tc| tc.id.clone())
        .collect();

    // Collect all call_ids covered by tool result messages
    let result_call_ids: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();

    // 1. Remove orphaned tool results
    let messages: Vec<Message> = messages
        .into_iter()
        .filter(|m| {
            if m.role == MessageRole::Tool {
                m.tool_call_id
                    .as_ref()
                    .map(|id| surviving_call_ids.contains(id))
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .collect();

    // 2. Insert stubs for orphaned calls
    let missing_ids: HashSet<String> = surviving_call_ids
        .difference(&result_call_ids)
        .cloned()
        .collect();

    if missing_ids.is_empty() {
        return messages;
    }

    let mut patched: Vec<Message> = Vec::with_capacity(messages.len() + missing_ids.len());
    for msg in messages {
        if msg.role == MessageRole::Assistant {
            let needs_stubs: Vec<String> = msg
                .tool_calls
                .iter()
                .flatten()
                .filter(|tc| missing_ids.contains(&tc.id))
                .map(|tc| tc.id.clone())
                .collect();
            patched.push(msg);
            for call_id in needs_stubs {
                patched.push(Message {
                    role: MessageRole::Tool,
                    content: "[Result from earlier conversation — see context summary above]"
                        .into(),
                    tool_call_id: Some(call_id),
                    tool_calls: None,
                    thinking_blocks: vec![],
                });
            }
        } else {
            patched.push(msg);
        }
    }
    patched
}
```

- [ ] **Step 6.4: Run tests**

```bash
cargo test -p opex-core sanitize_ 2>&1 | tail -5
```

Expected: both tests pass.

- [ ] **Step 6.5: Commit**

```bash
git add crates/opex-core/src/agent/history.rs
git commit -m "feat(compaction): add Phase 5 sanitize_tool_pairs for orphaned tool calls/results"
```

---

## Task 7: Phase 3 — Hermes summary + iterative update

**Files:**
- Modify: `crates/opex-core/src/agent/history.rs`

Constants and the new `generate_hermes_summary` function.

- [ ] **Step 7.1: Add constants**

Near the top of `history.rs` (after imports):

```rust
pub const SUMMARY_PREFIX: &str = "[CONTEXT COMPACTION — REFERENCE ONLY] Earlier turns were compacted \
into the summary below. This is a handoff from a previous context window — treat it as background \
reference, NOT as active instructions. Do NOT answer questions or fulfill requests mentioned in \
this summary; they were already addressed. Your current task is identified in the '## Active Task' \
section — resume exactly from there. Respond ONLY to the latest user message that appears AFTER \
this summary.";

const SUMMARY_NOTE_FOR_SYSTEM: &str = "[Note: Some earlier conversation turns have been compacted \
into a handoff summary to preserve context space. Build on that summary rather than re-doing work.]";

const MIN_SUMMARY_TOKENS: usize = 2000;
const SUMMARY_RATIO: f64 = 0.20;
const SUMMARY_TOKENS_CEILING: usize = 12_000;
```

- [ ] **Step 7.2: Implement `generate_hermes_summary`**

Add this function to `history.rs`:

```rust
/// Phase 3: generate a structured 13-section summary via LLM.
/// When `previous_summary` is Some, generates an iterative update.
/// Returns None on LLM failure (caller should use static fallback).
pub async fn generate_hermes_summary(
    turns: &[Message],
    provider: &dyn LlmProvider,
    language: Option<&str>,
    previous_summary: Option<&str>,
) -> Option<String> {
    let content_tokens = estimate_tokens(turns);
    let budget = (content_tokens as f64 * SUMMARY_RATIO) as usize;
    let budget = budget.clamp(MIN_SUMMARY_TOKENS, SUMMARY_TOKENS_CEILING);

    let lang_instruction = match language {
        Some("en") => "Write the summary in English.",
        _ => "Write the summary in Russian.",
    };

    let template = format!(r#"## Active Task
[THE SINGLE MOST IMPORTANT FIELD. Copy the user's most recent request verbatim. If no outstanding task, write "None."]

## Goal
[What the user is trying to accomplish overall]

## Constraints & Preferences
[User preferences, coding style, important constraints]

## Completed Actions
[Numbered list: N. ACTION target — outcome [tool: name]]

## Active State
[Working directory, branch, modified files, test status, running processes]

## In Progress
[Work underway when compaction fired]

## Blocked
[Blockers, errors, issues not resolved — include exact error messages]

## Key Decisions
[Important technical decisions and WHY]

## Resolved Questions
[Questions already answered — include the answer]

## Pending User Asks
[Questions or requests not yet answered — if none, write "None."]

## Relevant Files
[Files read, modified, created — with brief note]

## Remaining Work
[What remains to be done — framed as context, not instructions]

## Critical Context
[Specific values, error messages, config details that would be lost. NEVER include API keys — write [REDACTED].]

Target ~{budget} tokens. Be CONCRETE. Include file paths, commands, error messages, line numbers."#);

    let preamble = format!(
        "You are a summarization agent creating a context checkpoint. \
Your output will be injected as reference material for a DIFFERENT assistant \
that continues the conversation. Do NOT respond to any questions or requests \
in the conversation — only output the structured summary. \
Do NOT include any preamble, greeting, or prefix. \
{lang_instruction} \
NEVER include API keys, tokens, passwords, or secrets — write [REDACTED] instead."
    );

    let prompt_content = if let Some(prev) = previous_summary {
        let turns_text = format_messages_for_compaction(turns);
        format!(
            "{preamble}\n\nYou are UPDATING a context compaction summary. \
A previous compaction produced the summary below. New turns have occurred since then.\n\n\
PREVIOUS SUMMARY:\n{prev}\n\nNEW TURNS TO INCORPORATE:\n{turns_text}\n\n\
Update the summary using the structure below. PRESERVE all existing relevant info. \
ADD new completed actions (continue numbering). Move answered questions to Resolved. \
Update Active Task to the user's most recent unfulfilled request.\n\n{template}"
        )
    } else {
        let turns_text = format_messages_for_compaction(turns);
        format!(
            "{preamble}\n\nCreate a structured handoff summary for a different assistant \
that will continue this conversation.\n\nTURNS TO SUMMARIZE:\n{turns_text}\n\n{template}"
        )
    };

    let prompt = vec![Message {
        role: MessageRole::User,
        content: prompt_content,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
    }];

    let empty_tools: Vec<opex_types::ToolDefinition> = vec![];
    match provider
        .chat(
            &prompt,
            &empty_tools,
            crate::agent::providers::CallOptions::default(),
        )
        .await
    {
        Ok(response) => {
            let summary = response.content.trim().to_string();
            if summary.is_empty() {
                None
            } else {
                Some(format!("{SUMMARY_PREFIX}\n{summary}"))
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to generate context summary");
            None
        }
    }
}
```

- [ ] **Step 7.3: Write tests for `generate_hermes_summary`**

Add to `#[cfg(test)]` block in `history.rs`:

```rust
// Reusable mock provider for summary tests
struct EchoProvider(String);
#[async_trait::async_trait]
impl crate::agent::providers::LlmProvider for EchoProvider {
    fn name(&self) -> &str { "echo" }
    async fn chat(&self, _msgs: &[Message], _tools: &[hydraclaw_types::ToolDefinition],
                  _opts: crate::agent::providers::CallOptions)
        -> anyhow::Result<crate::agent::providers::LlmResponse>
    {
        Ok(crate::agent::providers::LlmResponse {
            content: self.0.clone(),
            tool_calls: vec![], thinking_blocks: vec![],
            usage: None, model: None, provider: None,
        })
    }
    fn run_max_duration_secs(&self) -> u64 { 30 }
    fn model_name(&self) -> &str { "echo-model" }
}

struct FailProvider;
#[async_trait::async_trait]
impl crate::agent::providers::LlmProvider for FailProvider {
    fn name(&self) -> &str { "fail" }
    async fn chat(&self, _msgs: &[Message], _tools: &[hydraclaw_types::ToolDefinition],
                  _opts: crate::agent::providers::CallOptions)
        -> anyhow::Result<crate::agent::providers::LlmResponse>
    {
        anyhow::bail!("simulated LLM failure")
    }
    fn run_max_duration_secs(&self) -> u64 { 30 }
    fn model_name(&self) -> &str { "fail-model" }
}

#[tokio::test]
async fn generate_hermes_summary_prepends_prefix() {
    use hydraclaw_types::MessageRole;
    let turns = vec![Message {
        role: MessageRole::User, content: "hello".into(),
        tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
    }];
    let provider = EchoProvider("My summary body".into());
    let result = generate_hermes_summary(&turns, &provider, None, None).await;
    let text = result.unwrap();
    assert!(text.starts_with(SUMMARY_PREFIX), "must start with SUMMARY_PREFIX");
    assert!(text.contains("My summary body"));
}

#[tokio::test]
async fn generate_hermes_summary_includes_previous_in_iterative_update() {
    use hydraclaw_types::MessageRole;
    let turns = vec![Message {
        role: MessageRole::User, content: "new turn".into(),
        tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
    }];
    // EchoProvider returns whatever is in its prompt — we inspect what was sent
    // by checking that the returned content includes the previous summary text.
    // Since EchoProvider echoes a fixed string we test the prompt was built with "UPDATING".
    // Use a provider that echoes the prompt back.
    struct PromptEchoProvider;
    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for PromptEchoProvider {
        fn name(&self) -> &str { "prompt-echo" }
        async fn chat(&self, msgs: &[Message], _tools: &[hydraclaw_types::ToolDefinition],
                      _opts: crate::agent::providers::CallOptions)
            -> anyhow::Result<crate::agent::providers::LlmResponse>
        {
            let content = msgs.first().map(|m| m.content.clone()).unwrap_or_default();
            Ok(crate::agent::providers::LlmResponse {
                content, tool_calls: vec![], thinking_blocks: vec![],
                usage: None, model: None, provider: None,
            })
        }
        fn run_max_duration_secs(&self) -> u64 { 30 }
        fn model_name(&self) -> &str { "prompt-echo-model" }
    }
    let provider = PromptEchoProvider;
    let prev = "PREVIOUS SUMMARY CONTENT";
    let result = generate_hermes_summary(&turns, &provider, None, Some(prev)).await;
    let text = result.unwrap();
    assert!(text.contains("UPDATING"), "iterative update prompt must contain UPDATING");
    assert!(text.contains(prev), "iterative update prompt must include previous summary");
}

#[tokio::test]
async fn generate_hermes_summary_returns_none_on_llm_failure() {
    use hydraclaw_types::MessageRole;
    let turns = vec![Message {
        role: MessageRole::User, content: "hello".into(),
        tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
    }];
    let provider = FailProvider;
    let result = generate_hermes_summary(&turns, &provider, None, None).await;
    assert!(result.is_none(), "must return None when LLM fails");
}
```

- [ ] **Step 7.4: Run tests**

```bash
cargo test -p opex-core generate_hermes_summary 2>&1 | tail -10
```

Expected: all 3 tests pass.

- [ ] **Step 7.5: Verify it compiles cleanly**

```bash
cd crates/opex-core && cargo check 2>&1 | grep "^error"
```

Expected: no errors.

- [ ] **Step 7.6: Commit**

```bash
git add crates/opex-core/src/agent/history.rs
git commit -m "feat(compaction): add Phase 3 generate_hermes_summary with 13-section template and iterative update"
```

---

## Task 8: Phase 4 — Assembly + `compress_messages` orchestrator

**Files:**
- Modify: `crates/opex-core/src/agent/history.rs`

- [ ] **Step 8.1: Write failing integration test**

```rust
#[tokio::test]
async fn compress_messages_reduces_token_count_and_keeps_tail() {
    // This test uses a mock provider that returns a fixed summary.
    // We build a 20-message conversation where the last 3 messages are recent.
    use opex_types::MessageRole;

    struct MockProvider;
    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for MockProvider {
        fn name(&self) -> &str { "mock" }
        async fn chat(&self, _msgs: &[Message], _tools: &[opex_types::ToolDefinition],
                      _opts: crate::agent::providers::CallOptions)
            -> anyhow::Result<crate::agent::providers::LlmResponse>
        {
            Ok(crate::agent::providers::LlmResponse {
                content: "Mock summary content".into(),
                tool_calls: vec![],
                thinking_blocks: vec![],
                usage: None,
                model: None,
                provider: None,
            })
        }
        fn run_max_duration_secs(&self) -> u64 { 30 }
        fn model_name(&self) -> &str { "mock-model" }
    }

    let mut msgs: Vec<Message> = (0..20).map(|i| Message {
        role: if i % 2 == 0 { MessageRole::User } else { MessageRole::Assistant },
        content: "word ".repeat(100), // ~100 tokens each
        tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
    }).collect();
    // Make last message a user message (invariant: last user must be in tail)
    msgs[19].role = MessageRole::User;

    let provider: &dyn crate::agent::providers::LlmProvider = &MockProvider;
    let cfg = crate::config::CompactionConfig {
        enabled: true,
        threshold: 0.75,
        protect_first_n: 3,
        preserve_last_n: 3,
        summary_target_ratio: 0.20,
        extract_to_memory: false, // skip pgvector in unit test
        ..Default::default()
    };

    let tokens_before = estimate_tokens(&msgs) as u32;
    let mut compressor = crate::agent::compressor::Compressor::new(200_000);

    let facts = compress_messages(&mut msgs, &mut compressor, &cfg, provider, None).await.unwrap();

    let tokens_after = estimate_tokens(&msgs) as u32;
    assert!(tokens_after < tokens_before, "compression must reduce tokens");
    // Last 3 messages preserved
    assert!(msgs.len() >= 3);
    assert_eq!(msgs.last().unwrap().role, MessageRole::User);
    // Summary stored in compressor
    assert!(compressor.previous_summary.is_some());
    assert_eq!(compressor.compression_count, 1);
    // extract_to_memory = false → empty facts
    assert!(facts.is_empty());
}
```

- [ ] **Step 8.2: Run to confirm it fails**

```bash
cargo test -p opex-core compress_messages_reduces 2>&1 | tail -5
```

Expected: compile error (`compress_messages` not found).

- [ ] **Step 8.3a: Add `extract_facts_only` helper** (before `compress_messages`)

This is a standalone fact-extraction function that avoids the `usize::MAX` overflow
in `compact_if_needed`. Add to `history.rs`:

```rust
/// Extract facts from a conversation slice into memory-ready strings.
/// Called in parallel with summary generation during Phase 3.
/// Returns empty Vec on failure (non-fatal).
async fn extract_facts_only(
    turns: &[Message],
    provider: &dyn LlmProvider,
    language: Option<&str>,
) -> Vec<String> {
    let lang_hint = match language {
        Some("ru") => " Write each fact in Russian.",
        Some("en") => " Write each fact in English.",
        _ => "",
    };
    let formatted = format_messages_for_compaction(turns);
    let extraction_prompt = vec![
        Message {
            role: MessageRole::System,
            content: format!(
                "Extract key facts from this conversation as a JSON array of strings.\n\
MUST PRESERVE: active tasks with progress, UUIDs/URLs/file paths/IPs, decisions \
and rationale, user preferences, error conditions and resolutions, commitments.\n\
MAY OMIT: routine greetings, tool calls without noteworthy results, repeated info.\n\
Each fact must be self-contained.{lang_hint}\n\
Return ONLY the JSON array, no other text."
            ),
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
        },
        Message {
            role: MessageRole::User,
            content: formatted,
            tool_calls: None, tool_call_id: None, thinking_blocks: vec![],
        },
    ];
    let empty_tools: Vec<hydraclaw_types::ToolDefinition> = vec![];
    match provider.chat(&extraction_prompt, &empty_tools,
                        crate::agent::providers::CallOptions::default()).await {
        Ok(resp) => serde_json::from_str::<Vec<String>>(&resp.content).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "fact extraction failed, skipping");
            vec![]
        }
    }
}
```

- [ ] **Step 8.3b: Implement `compress_messages` orchestrator**

```rust
/// Main entry point: run all 5 phases of compression on `messages`.
/// `language` should be `engine.cfg().agent.language.as_deref()`.
/// `db_pool` is optional — when Some and `cfg.extract_to_memory = true`,
/// extracted facts are returned for the caller to store in pgvector.
pub async fn compress_messages(
    messages: &mut Vec<Message>,
    compressor: &mut crate::agent::compressor::Compressor,
    cfg: &crate::config::CompactionConfig,
    provider: &dyn LlmProvider,
    language: Option<&str>,
) -> anyhow::Result<Vec<String>> {   // returns extracted facts (empty if disabled)
    let tokens_before = estimate_tokens(messages) as u32;

    // Phase 1: pre-pass — prune + deduplicate tool results
    let pruned = prune_old_tool_results(messages, cfg.preserve_last_n);

    // Phase 2: boundaries
    let head_end = find_head_end(&pruned, cfg.protect_first_n);
    let tail_budget = (compressor.context_limit as f64
        * cfg.threshold
        * cfg.summary_target_ratio) as usize;
    let tail_start = find_tail_start_by_tokens(&pruned, head_end, tail_budget);

    if head_end >= tail_start {
        tracing::debug!(
            "compression skipped: head_end={head_end} >= tail_start={tail_start}"
        );
        return Ok(vec![]);
    }

    let turns_to_summarize: Vec<Message> = pruned[head_end..tail_start].to_vec();
    let tail: Vec<Message> = pruned[tail_start..].to_vec();
    let head: Vec<Message> = pruned[..head_end].to_vec();

    // Phase 3: LLM summary + fact extraction — parallel on a read-only snapshot
    let previous = compressor.previous_summary.as_deref();
    let (summary, facts) = tokio::join!(
        generate_hermes_summary(&turns_to_summarize, provider, language, previous),
        async {
            if cfg.extract_to_memory {
                extract_facts_only(&turns_to_summarize, provider, language).await
            } else {
                vec![]
            }
        }
    );

    // Fallback if LLM failed
    let summary_text = summary.unwrap_or_else(|| {
        let n = turns_to_summarize.len();
        tracing::warn!(n, "summary generation failed — inserting static fallback");
        format!(
            "{SUMMARY_PREFIX}\nSummary generation was unavailable. \
{n} message(s) were removed to free context space. \
Continue based on the recent messages below."
        )
    });

    // Fallback if LLM failed
    let summary_text = summary.unwrap_or_else(|| {
        let n = turns_to_summarize.len();
        tracing::warn!(n, "summary generation failed — inserting static fallback");
        format!(
            "{SUMMARY_PREFIX}\nSummary generation was unavailable. \
{n} message(s) were removed to free context space. \
Continue based on the recent messages below."
        )
    });

    // Phase 4: assemble head + summary message + tail
    let summary_role = if head
        .last()
        .map(|m| matches!(m.role, MessageRole::Assistant) || m.role == MessageRole::Tool)
        .unwrap_or(false)
    {
        MessageRole::User
    } else {
        MessageRole::Assistant
    };

    let mut assembled: Vec<Message> = Vec::with_capacity(head.len() + 1 + tail.len());

    // Append one-time note to system message (index 0 of head)
    for (i, mut msg) in head.into_iter().enumerate() {
        if i == 0 && msg.role == MessageRole::System && !msg.content.contains(SUMMARY_NOTE_FOR_SYSTEM) {
            msg.content.push_str(&format!("\n\n{SUMMARY_NOTE_FOR_SYSTEM}"));
        }
        assembled.push(msg);
    }

    // Check if summary role collides with first tail message
    let first_tail_role = tail.first().map(|m| m.role.clone());
    let (role_for_summary, merge_into_tail) = match first_tail_role {
        Some(ref tr) if *tr == summary_role => {
            // Both collide with head — merge into first tail message
            let opposite = if summary_role == MessageRole::User {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let head_last_role = assembled.last().map(|m| m.role.clone());
            if head_last_role.as_ref() != Some(&opposite) {
                (opposite, false)
            } else {
                (summary_role.clone(), true)
            }
        }
        _ => (summary_role, false),
    };

    if !merge_into_tail {
        assembled.push(Message {
            role: role_for_summary,
            content: summary_text.clone(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        });
        assembled.extend(tail);
    } else {
        let mut tail_iter = tail.into_iter();
        if let Some(mut first_tail) = tail_iter.next() {
            first_tail.content = format!(
                "{summary_text}\n\n--- END OF CONTEXT SUMMARY — respond to the message below ---\n\n{}",
                first_tail.content
            );
            assembled.push(first_tail);
        }
        assembled.extend(tail_iter);
    }

    // Phase 5: sanitize tool pairs
    let assembled = sanitize_tool_pairs(assembled);

    // Commit result
    *messages = assembled;
    compressor.previous_summary = Some(summary_text);

    let tokens_after = estimate_tokens(messages) as u32;
    compressor.record_compression_result(tokens_before, tokens_after, cfg);

    tracing::info!(
        tokens_before,
        tokens_after,
        msgs_after = messages.len(),
        compression_count = compressor.compression_count,
        "compress_messages complete"
    );

    Ok(())
}
```

- [ ] **Step 8.4: Run tests**

```bash
cargo test -p opex-core compress_messages 2>&1 | tail -10
```

Expected: test passes.

- [ ] **Step 8.5: Run full test suite**

```bash
cargo test -p opex-core 2>&1 | grep -E "FAILED|test result"
```

Expected: 0 failures (DB tests that need DATABASE_URL will skip).

- [ ] **Step 8.6: Commit**

```bash
git add crates/opex-core/src/agent/history.rs
git commit -m "feat(compaction): add compress_messages orchestrator — Phase 4 assembly + full 5-phase flow"
```

---

## Task 9: Bootstrap — load compaction state

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/bootstrap.rs`

- [ ] **Step 9.1: Add `compressor` to `BootstrapOutcome`**

Find `pub struct BootstrapOutcome` in `bootstrap.rs` and add:

```rust
/// Per-session compressor state loaded from DB (or fresh if first session).
pub compressor: crate::agent::compressor::Compressor,
```

- [ ] **Step 9.2: Compute `context_limit` before all return paths**

Run `grep -n "BootstrapOutcome {" crates/opex-core/src/agent/pipeline/bootstrap.rs` to find all construction sites. There will be 2-4 sites (normal path + early-return slash-command path + error path).

Add this block **before the first `BootstrapOutcome {`** construction site AND before any early returns:

```rust
// Compute once — used in all BootstrapOutcome construction sites below
let context_limit = crate::agent::pipeline::llm_call::default_context_for_model(
    &engine.cfg().agent.model
) as u32;
```

- [ ] **Step 9.3: Load compaction state for the normal path**

Immediately after `context_limit` is computed (and after `session_id` is finalized), add:

```rust
// Load compaction state — fresh Compressor if first session or parse fails
let compaction_state = crate::db::compaction::get_compaction_state(
    &engine.cfg().db, session_id,
).await.unwrap_or(None);
let compressor = crate::agent::compressor::Compressor::load(compaction_state, context_limit);
```

- [ ] **Step 9.4: Add `compressor` field to every `BootstrapOutcome { ... }` literal**

For the **normal return path** use the loaded `compressor` variable.
For **early-return paths** (slash-command handled, session not yet created), use a fresh instance:

```rust
// Early-return path (e.g. slash-command handled before session creation):
BootstrapOutcome {
    // ... existing fields ...
    compressor: crate::agent::compressor::Compressor::new(context_limit),
}

// Normal path (session created, state loaded from DB):
BootstrapOutcome {
    // ... existing fields ...
    compressor,
}
```

- [ ] **Step 9.5: Verify compilation**

```bash
cargo check -p opex-core 2>&1 | grep "^error"
```

Expected: no errors (all BootstrapOutcome construction sites updated).

- [ ] **Step 9.6: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/bootstrap.rs
git commit -m "feat(compaction): load compaction_state in bootstrap, add Compressor to BootstrapOutcome"
```

---

## Task 10: Execute — proactive trigger

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/execute.rs`

- [ ] **Step 10.1: Find the function signature of `execute`**

```bash
grep -n "pub async fn execute" crates/opex-core/src/agent/pipeline/execute.rs | head -3
```

- [ ] **Step 10.2: Add `compressor` to `execute` signature**

Run `grep -n "pub async fn execute" crates/opex-core/src/agent/pipeline/execute.rs | head -3`
to find the current signature. It will look like:

```rust
pub async fn execute(
    engine: &AgentEngine,
    ctx: ExecuteContext,          // or individual parameters
    mut messages: Vec<Message>,
    // ... other params ...
    sm: &SessionManager,
) -> anyhow::Result<ExecuteOutcome>
```

Add `compressor: &mut crate::agent::compressor::Compressor` as the last parameter before
the return type:

```rust
pub async fn execute(
    engine: &AgentEngine,
    ctx: ExecuteContext,
    mut messages: Vec<Message>,
    // ... other existing params unchanged ...
    sm: &SessionManager,
    compressor: &mut crate::agent::compressor::Compressor,
) -> anyhow::Result<ExecuteOutcome>
```

- [ ] **Step 10.3: Update all callers of `execute`**

Run `grep -rn "::execute\b\|pipeline::execute(" crates/opex-core/src/agent/ | grep -v "test\|#\["`.

Each call site is in `engine/run.rs` (3 adapter functions: `handle_sse`, `handle_with_status`,
`handle_streaming`). In each, `BootstrapOutcome` is already destructured or accessed.
Add `&mut outcome.compressor` as the final argument:

```rust
// Before:
execute(engine, ctx, messages, tools, loop_detector, session_id, sm).await?

// After:
execute(engine, ctx, messages, tools, loop_detector, session_id, sm,
        &mut outcome.compressor).await?
```

- [ ] **Step 10.4: Add proactive trigger before LLM call**

In `execute.rs`, find the comment `// 3. Compact tool results` (around line 166) and `// 4. Call LLM` (around line 174). Between these two, add the proactive check:

```rust
// 3. Compact tool results (existing path — kept as backstop)
crate::agent::pipeline::context::compact_tool_results(
    &engine.cfg().agent.model,
    engine.cfg().agent.compaction.as_ref(),
    &mut messages,
    &mut context_chars,
);

// 3b. Proactive compression: check token budget from last response
if let Some(cmp_cfg) = engine.cfg().agent.compaction.as_ref().filter(|c| c.enabled) {
    if compressor.should_compress(cmp_cfg) {
        let active_provider: &dyn crate::agent::providers::LlmProvider =
            engine.cfg().compaction_provider
                .as_deref()
                .unwrap_or_else(|| engine.cfg().provider.as_ref());
        match crate::agent::history::compress_messages(
            &mut messages,
            compressor,
            cmp_cfg,
            active_provider,
            engine.cfg().agent.language.as_deref(),
        )
        .await
        {
            Ok(facts) if !facts.is_empty() => {
                // Store extracted facts in pgvector (fire-and-forget)
                let db = engine.cfg().db.clone();
                let agent = engine.cfg().agent.name.clone();
                tokio::spawn(async move {
                    // Fact storage uses existing memory pipeline
                    // (same as compact_if_needed did previously)
                    tracing::debug!(count = facts.len(), agent = %agent, "storing extracted facts");
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "proactive compression failed, continuing");
            }
        }
    }
}
```

- [ ] **Step 10.5: Update token count after each LLM response**

Find where `response.usage` is consumed (around line 246 where `StreamEvent::Usage` is emitted). After the usage event emission, add:

```rust
// Update compressor with real token count for next proactive check
if let Some(ref usage) = response.usage {
    compressor.update_token_count(usage.input_tokens);
}
```

- [ ] **Step 10.6: Verify compilation**

```bash
cargo check -p opex-core 2>&1 | grep "^error"
```

Expected: no errors.

- [ ] **Step 10.7: Run tests**

```bash
cargo test -p opex-core 2>&1 | grep -E "FAILED|test result" | tail -5
```

Expected: 0 new failures.

- [ ] **Step 10.8: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/execute.rs
git commit -m "feat(compaction): add proactive compression trigger in execute.rs before each LLM call"
```

---

## Task 11: Finalize — save compaction state

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/finalize.rs`

- [ ] **Step 11.1: Find finalize function signature**

```bash
grep -n "pub async fn finalize\|pub fn finalize" crates/opex-core/src/agent/pipeline/finalize.rs | head -5
```

- [ ] **Step 11.2: Add `compressor` parameter to finalize signature**

Run `grep -n "pub async fn finalize\|pub fn finalize" crates/opex-core/src/agent/pipeline/finalize.rs | head -5`
to find the function signature. It accesses `session_id` and a `db: &PgPool`. Add `compressor`:

```rust
pub async fn finalize(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    // ... other existing params unchanged ...
    compressor: &crate::agent::compressor::Compressor,
) -> anyhow::Result<FinalizeOutcome>   // return type may differ — match what's there
```

- [ ] **Step 11.3: Save state after session status is written**

Locate the line that writes `run_status = 'done'` or `'failed'` to the sessions table.
Add immediately after that `sqlx::query` call completes:

```rust
// Persist compaction state — non-fatal if it fails
let state_json = compressor.to_json();
if let Err(e) = crate::db::compaction::set_compaction_state(
    db, session_id, state_json,
).await {
    tracing::warn!(
        error = %e,
        session_id = %session_id,
        "failed to save compaction_state — next session starts fresh"
    );
}
```

- [ ] **Step 11.4: Update all callers of finalize**

Run `grep -rn "pipeline::finalize\|::finalize(" crates/opex-core/src/agent/ | grep -v "test\|#\["`.

Callers are in `engine/run.rs` (same 3 adapter functions as execute). At each call site,
`compressor` is available from `outcome.compressor` (owned after `execute` returns).
Pass a reference:

```rust
// Before:
finalize(db, session_id, outcome, sm).await?

// After:
finalize(db, session_id, outcome, sm, &compressor).await?
```

Where `compressor` is obtained by moving out of `outcome` before the call:
```rust
let compressor = outcome.compressor;  // move out (execute takes &mut, so it's done)
finalize(db, session_id, /* ... */, &compressor).await?
```

- [ ] **Step 11.5: Final compilation check**

```bash
cargo check -p opex-core 2>&1 | grep "^error"
```

Expected: no errors.

- [ ] **Step 11.6: Full test suite**

```bash
cargo test -p opex-core 2>&1 | grep -E "FAILED|test result"
```

Expected: 0 new failures.

- [ ] **Step 11.7: Clippy**

```bash
cargo clippy -p opex-core -- -D warnings 2>&1 | grep "^error" | head -10
```

Fix any errors before committing.

- [ ] **Step 11.8: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/finalize.rs
git commit -m "feat(compaction): save compaction_state in finalize.rs for session persistence"
```

---

## Task 12: Build verification + deploy

- [ ] **Step 12.1: ARM64 build**

```bash
cargo zigbuild --target aarch64-unknown-linux-gnu --release 2>&1 | tail -5
```

Expected: `Finished release`.

- [ ] **Step 12.2: Deploy to Pi**

```bash
PI_HOST="aronmav@192.168.1.85"
PI_DIR="~/opex"
TARGET="aarch64-unknown-linux-gnu"

ssh $PI_HOST "systemctl --user stop opex-core opex-watchdog opex-memory-worker"
scp target/$TARGET/release/opex-core "$PI_HOST:$PI_DIR/opex-core-aarch64"
ssh $PI_HOST "chmod +x $PI_DIR/opex-core-aarch64; systemctl --user start opex-core opex-watchdog opex-memory-worker"
```

- [ ] **Step 12.3: Health check**

```bash
sleep 5
AUTH=$(cat .auth-token)
ssh aronmav@192.168.1.85 "curl -sf -H 'Authorization: Bearer $AUTH' http://localhost:18789/api/doctor" | python3 -m json.tool | grep '"ok"'
```

Expected: `"ok": true` (or same baseline as before — only `ollama-default` failing).

- [ ] **Step 12.4: Smoke test — send a message and verify no crash**

```bash
AUTH=$(cat .auth-token)
curl -sf -H "Authorization: Bearer $AUTH" \
  -H "Content-Type: application/json" \
  -d '{"agent":"Arty","text":"ping"}' \
  "http://192.168.1.85:18789/api/chat" | head -3
```

Expected: SSE stream starts (first event is `data-session-id`).

- [ ] **Step 12.5: Verify commit history**

```bash
git log --oneline -12
```

Expected: 11 feature commits visible (Tasks 1–11 each produce one commit).

---

## Self-Review

**Spec coverage check:**

| Spec requirement | Task |
|---|---|
| `sessions.compaction_state JSONB` migration | Task 1 |
| `get/set_compaction_state` DB helpers | Task 1 |
| 5 new `CompactionConfig` fields | Task 2 |
| `Compressor` struct + `should_compress` | Task 3 |
| Anti-thrashing (`record_compression_result`) | Task 3 |
| Phase 1: prune old tool results + dedup | Task 4 |
| Phase 2: `find_head_end`, `find_tail_start_by_tokens` | Task 5 |
| Phase 2: last user message invariant | Task 5 |
| Phase 5: `sanitize_tool_pairs` | Task 6 |
| Phase 3: `generate_hermes_summary` with 13 sections | Task 7 |
| Phase 3: iterative update via `previous_summary` | Task 7 |
| Phase 3: `SUMMARY_PREFIX` constant | Task 7 |
| Phase 3: provider fallback (compaction_provider) | Task 8 |
| Phase 3: parallel fact extraction | Task 8 |
| Phase 4: assembly with role logic | Task 8 |
| `compress_messages` orchestrator | Task 8 |
| Bootstrap: load state into `BootstrapOutcome` | Task 9 |
| Execute: proactive trigger before LLM call | Task 10 |
| Execute: `update_token_count` after response | Task 10 |
| Finalize: save state to DB | Task 11 |
| Known limitation: crash between compress and finalize | Noted in spec, no code needed |
| `enabled = false` default (backward compat) | Inherited from existing `Option<CompactionConfig>` |
