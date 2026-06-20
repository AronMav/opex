# Hermes-parity Phase 2a — Implementation Plan (`/voice` + `/compact`-UI)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface `/compact` in the chat slash menu, and add a `/voice on|off|status` per-chat toggle that auto-sends each agent reply as a voice message.

**Architecture:** G is a one-line UI list addition + i18n. F adds a DB-backed per-chat voice mode (`channel_voice_modes`), a `/voice` slash command in the existing command dispatcher, and a post-pipeline auto-TTS hook in the channel entry path that reuses the existing `synthesize_speech` YAML tool + `send_voice` channel-action machinery.

**Tech Stack:** Rust 2024 (sqlx, tokio), PostgreSQL 17, Next.js/React + vitest.

## Global Constraints

- rustls-only; `cargo clippy --bin hydeclaw-core --all-targets -- -D warnings` must pass.
- Application-tree Rust tests run under `cargo test --bin hydeclaw-core` (the lib facade excludes `gateway::handlers`/`agent`; the bin compiles the full tree). DB tests need the test postgres (`docker compose -f docker/docker-compose.test.yml up -d --build postgres-test`, `DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test`).
- `#[sqlx::test]` must use `#[sqlx::test(migrations = "../../migrations")]` or the schema is empty.
- Migrations runtime-loaded; `make remote-deploy` now syncs them.
- Commit messages: conventional, no `Co-Authored-By`. No `git push` unless the user asks.
- UI tests: `cd ui && npm test`.

---

### Task 1: `/compact` in the chat slash menu (Component G)

**Files:**
- Modify: `ui/src/app/(authenticated)/chat/parts/SlashMenu.tsx` (`SLASH_COMMAND_KEYS` at line 5)
- Modify: `ui/src/i18n/locales/en.json`, `ui/src/i18n/locales/ru.json` (after the `chat.slash_reset` key, line 288)
- Create: `ui/src/__tests__/slash-menu.test.tsx`

**Interfaces:**
- Produces: a `/compact` entry visible when the query is a prefix of `/compact`.

- [ ] **Step 1: Write the failing test** — `ui/src/__tests__/slash-menu.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { SlashMenu } from "@/app/(authenticated)/chat/parts/SlashMenu";

describe("SlashMenu", () => {
  it("offers /compact when typing /comp", () => {
    render(<SlashMenu query="/comp" onSelect={() => {}} onClose={() => {}} />);
    expect(screen.getByText("/compact")).toBeInTheDocument();
  });

  it("hides /compact when query does not match", () => {
    render(<SlashMenu query="/think" onSelect={() => {}} onClose={() => {}} />);
    expect(screen.queryByText("/compact")).not.toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd ui && npm test -- slash-menu`
Expected: FAIL — `/compact` not found (no entry yet).

- [ ] **Step 3: Add the command entry** — in `SlashMenu.tsx`, add to `SLASH_COMMAND_KEYS` (after the `/reset` line):

```tsx
  { cmd: "/compact", key: "chat.slash_compact" },
```

- [ ] **Step 4: Add the i18n keys** — in `en.json` after `"chat.slash_reset": ...`:

```json
  "chat.slash_compact": "Compress conversation history",
```
in `ru.json` after `"chat.slash_reset": ...`:

```json
  "chat.slash_compact": "Сжать историю разговора",
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cd ui && npm test -- slash-menu`
Expected: PASS (both tests).

- [ ] **Step 6: Commit**

```bash
git add "ui/src/app/(authenticated)/chat/parts/SlashMenu.tsx" ui/src/i18n/locales/en.json ui/src/i18n/locales/ru.json ui/src/__tests__/slash-menu.test.tsx
git commit -m "feat(ui/chat): surface /compact in the slash menu autocomplete"
```

---

### Task 2: `channel_voice_modes` table + storage (Component F, part 1)

**Files:**
- Create: `migrations/055_channel_voice_modes.sql`
- Create: `crates/hydeclaw-core/src/db/channel_voice_modes.rs`
- Modify: `crates/hydeclaw-core/src/db/mod.rs` (add `pub mod channel_voice_modes;`)

**Interfaces:**
- Produces: `get_voice_mode(db, channel, chat_id) -> Result<String>` (`"off"` when absent), `set_voice_mode(db, channel, chat_id, mode) -> Result<()>`.

- [ ] **Step 1: Create the migration** `migrations/055_channel_voice_modes.sql`:

```sql
CREATE TABLE channel_voice_modes (
    channel    TEXT NOT NULL,
    chat_id    TEXT NOT NULL,
    mode       TEXT NOT NULL DEFAULT 'off'
               CHECK (mode IN ('off', 'on')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel, chat_id)
);
```

- [ ] **Step 2: Register the module** — in `db/mod.rs`, in the "Remaining modules" group:

```rust
pub mod channel_voice_modes;
```

- [ ] **Step 3: Write the failing DB test** — create `crates/hydeclaw-core/src/db/channel_voice_modes.rs` with the test first (impl in Step 5):

```rust
//! Per-chat voice-mode storage (table `channel_voice_modes`).

use anyhow::Result;
use sqlx::PgPool;

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn defaults_to_off_then_roundtrips(pool: PgPool) -> sqlx::Result<()> {
        assert_eq!(get_voice_mode(&pool, "telegram", "42").await.unwrap(), "off");
        set_voice_mode(&pool, "telegram", "42", "on").await.unwrap();
        assert_eq!(get_voice_mode(&pool, "telegram", "42").await.unwrap(), "on");
        set_voice_mode(&pool, "telegram", "42", "off").await.unwrap();
        assert_eq!(get_voice_mode(&pool, "telegram", "42").await.unwrap(), "off");
        Ok(())
    }
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test cargo test --bin hydeclaw-core channel_voice_modes`
Expected: FAIL — `get_voice_mode` / `set_voice_mode` not found.

- [ ] **Step 5: Implement the storage functions** (insert above the `#[cfg(test)]` block):

```rust
/// Current voice mode for a chat (`"on"` / `"off"`). Returns `"off"` when unset.
pub async fn get_voice_mode(db: &PgPool, channel: &str, chat_id: &str) -> Result<String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT mode FROM channel_voice_modes WHERE channel = $1 AND chat_id = $2",
    )
    .bind(channel)
    .bind(chat_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(m,)| m).unwrap_or_else(|| "off".to_string()))
}

/// Upsert the voice mode for a chat.
pub async fn set_voice_mode(db: &PgPool, channel: &str, chat_id: &str, mode: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO channel_voice_modes (channel, chat_id, mode)
         VALUES ($1, $2, $3)
         ON CONFLICT (channel, chat_id)
         DO UPDATE SET mode = EXCLUDED.mode, updated_at = now()",
    )
    .bind(channel)
    .bind(chat_id)
    .bind(mode)
    .execute(db)
    .await?;
    Ok(())
}
```

- [ ] **Step 6: Run to verify it passes**

Run: `DATABASE_URL=postgres://hydeclaw_test:hydeclaw_test@127.0.0.1:5434/hydeclaw_test cargo test --bin hydeclaw-core channel_voice_modes`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add migrations/055_channel_voice_modes.sql crates/hydeclaw-core/src/db/channel_voice_modes.rs crates/hydeclaw-core/src/db/mod.rs
git commit -m "feat(voice): channel_voice_modes table + get/set storage"
```

---

### Task 3: `/voice` slash command (Component F, part 2)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/commands.rs` (add `parse_voice_command` + a `"/voice"` match arm in `handle_command`)

**Interfaces:**
- Consumes: `db::channel_voice_modes::{get_voice_mode, set_voice_mode}`; `CommandContext.db`; `msg: &IncomingMessage` (already a `handle_command` parameter); `msg.channel`, `msg.context["chat_id"]`.
- Produces: `parse_voice_command(arg: &str) -> VoiceCmd` where `enum VoiceCmd { Status, Set(&'static str) }`.

- [ ] **Step 1: Write the failing unit test** — add to the `mod tests` block in `commands.rs` (pure, no DB):

```rust
    #[test]
    fn parse_voice_command_maps_args() {
        assert!(matches!(parse_voice_command("on"), VoiceCmd::Set("on")));
        assert!(matches!(parse_voice_command("off"), VoiceCmd::Set("off")));
        assert!(matches!(parse_voice_command(""), VoiceCmd::Status));
        assert!(matches!(parse_voice_command("status"), VoiceCmd::Status));
        assert!(matches!(parse_voice_command("garbage"), VoiceCmd::Status));
    }
```

(If `commands.rs` has no `mod tests`, add `#[cfg(test)] mod tests { use super::*; ... }` at the end of the file.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --bin hydeclaw-core parse_voice_command_maps_args`
Expected: FAIL — `parse_voice_command` / `VoiceCmd` not found.

- [ ] **Step 3: Add the parser** near the top of `commands.rs` (after the imports):

```rust
/// Parsed `/voice` argument.
pub enum VoiceCmd {
    Status,
    Set(&'static str),
}

/// Map a `/voice` argument to an action. Unknown args fall back to `Status`.
pub fn parse_voice_command(arg: &str) -> VoiceCmd {
    match arg.trim().to_lowercase().as_str() {
        "on" => VoiceCmd::Set("on"),
        "off" => VoiceCmd::Set("off"),
        _ => VoiceCmd::Status,
    }
}
```

- [ ] **Step 4: Add the `/voice` match arm** in `handle_command` (alongside the other arms, e.g. after `"/think"`):

```rust
        "/voice" => {
            let chat_id = msg
                .context
                .get("chat_id")
                .map(|v| v.to_string().trim_matches('"').to_string())
                .filter(|s| !s.is_empty() && s != "null");
            let Some(chat_id) = chat_id else {
                return Some(Ok("/voice only applies to chat channels (Telegram, etc.).".to_string()));
            };
            let channel = msg.channel.as_str();
            match parse_voice_command(args) {
                VoiceCmd::Set(mode) => {
                    if let Err(e) = crate::db::channel_voice_modes::set_voice_mode(ctx.db, channel, &chat_id, mode).await {
                        return Some(Ok(format!("Failed to set voice mode: {e}")));
                    }
                    let reply = if mode == "on" {
                        "Voice replies enabled for this chat. Each reply will also be sent as audio. /voice off to disable."
                    } else {
                        "Voice replies disabled for this chat."
                    };
                    Some(Ok(reply.to_string()))
                }
                VoiceCmd::Status => {
                    let mode = crate::db::channel_voice_modes::get_voice_mode(ctx.db, channel, &chat_id)
                        .await
                        .unwrap_or_else(|_| "off".to_string());
                    Some(Ok(format!("Voice mode for this chat: {mode}. Use /voice on or /voice off.")))
                }
            }
        }
```

- [ ] **Step 5: Run to verify it passes (parser test compiles + the arm compiles)**

Run: `cargo test --bin hydeclaw-core parse_voice_command_maps_args`
Expected: PASS (the parser test) and the crate compiles with the new arm.

- [ ] **Step 6: Lint + commit**

```bash
cargo clippy --bin hydeclaw-core --all-targets -- -D warnings
git add crates/hydeclaw-core/src/agent/pipeline/commands.rs
git commit -m "feat(voice): /voice on|off|status slash command (per-chat, DB-backed)"
```

---

### Task 4: Auto-TTS hook in the channel path (Component F, part 3)

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/engine/run.rs` (`handle_with_status`, after `finalize` at ~line 322)

**Interfaces:**
- Consumes: `db::channel_voice_modes::get_voice_mode`; `pipeline::CommandContext { cfg, state, tex, subagent_depth }`; `crate::tools::yaml_tools::find_yaml_tool(workspace_dir, name)`; `crate::agent::pipeline::channel_actions::execute_yaml_channel_action(ctx, tool, args, ca)`; the `synthesize_speech` YAML tool (input param `text`, `channel_action: send_voice`).

- [ ] **Step 1: Add a private helper** to the `impl AgentEngine` block in `run.rs` (above `handle_with_status`):

```rust
    /// If the chat has voice mode `on`, dispatch the final assistant text as a
    /// voice message by reusing the `synthesize_speech` YAML tool's channel-action
    /// path (background TTS → `send_voice`). Best-effort: never blocks or fails the turn.
    async fn maybe_auto_tts(&self, msg: &IncomingMessage, final_text: &str) {
        if final_text.trim().is_empty() {
            return;
        }
        let chat_id = match msg.context.get("chat_id") {
            Some(v) => v.to_string().trim_matches('"').to_string(),
            None => return, // web/UI turn — no chat to voice
        };
        if chat_id.is_empty() || chat_id == "null" {
            return;
        }
        let mode = crate::db::channel_voice_modes::get_voice_mode(&self.cfg().db, &msg.channel, &chat_id)
            .await
            .unwrap_or_else(|_| "off".to_string());
        if mode != "on" {
            return;
        }
        let tool = match crate::tools::yaml_tools::find_yaml_tool(&self.cfg().workspace_dir, "synthesize_speech").await {
            Some(t) => t,
            None => {
                tracing::warn!("auto-tts: synthesize_speech tool not found");
                return;
            }
        };
        let Some(ca) = tool.channel_action.clone() else {
            tracing::warn!("auto-tts: synthesize_speech has no channel_action");
            return;
        };
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: self.cfg(),
            state: self.state(),
            tex: self.tex(),
            subagent_depth: 0,
        };
        let args = serde_json::json!({ "text": final_text, "_context": msg.context });
        let result = crate::agent::pipeline::channel_actions::execute_yaml_channel_action(&ctx, &tool, &args, &ca).await;
        tracing::debug!(channel = %msg.channel, "auto-tts dispatched: {result}");
    }
```

- [ ] **Step 2: Call the helper** after `finalize` in `handle_with_status` (replace the `result` tail at ~line 321-324):

```rust
        let result =
            finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await;
        self.maybe_trim_session(session_id).await;
        if let Ok(ref final_text) = result {
            self.maybe_auto_tts(msg, final_text).await;
        }
        result
```

- [ ] **Step 3: Verify it compiles** (resolve exact field/fn names against the codebase):

Run: `cargo test --bin hydeclaw-core --no-run`
Expected: clean. If `tool.channel_action` is named differently or `find_yaml_tool` is sync, adjust to match `crate::tools::yaml_tools` (the `synthesize_speech.yaml` loader). `CommandContext` fields are exactly `cfg, state, tex, subagent_depth` (see `engine/mod.rs:341`).

- [ ] **Step 4: Lint**

Run: `cargo clippy --bin hydeclaw-core --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/hydeclaw-core/src/agent/engine/run.rs
git commit -m "feat(voice): auto-send each reply as a voice message when chat voice mode is on"
```

---

## Final verification & deploy

- [ ] `cd ui && npm test` — slash-menu test green.
- [ ] `cargo clippy --bin hydeclaw-core --all-targets -- -D warnings` — clean.
- [ ] Test postgres up, then `DATABASE_URL=… cargo test --bin hydeclaw-core` — full bin suite (incl. `channel_voice_modes`, `parse_voice_command`) green.
- [ ] Deploy: `make remote-deploy` (syncs migration 055) + UI build/deploy for G + `make doctor`.
- [ ] Server smoke: in a Telegram chat send `/voice on` → expect "enabled" reply; send a normal message → reply arrives as text **and** a voice message; `/voice off` → text only; `/voice` → reports current mode.

## Self-review checklist (completed by plan author)

- **Spec coverage:** G→Task 1; F-storage→Task 2; F-command→Task 3; F-hook→Task 4. All spec components mapped. Deferred items (`/voice only`, token-delta, `/goal`) intentionally absent.
- **Placeholder scan:** no TBD/TODO; code shown for every step. Task 4 Step 3 notes a compile-time field-name reconciliation against `yaml_tools` (the only runtime-resolved detail), not a placeholder.
- **Type consistency:** `get_voice_mode`/`set_voice_mode` (channel, chat_id, mode order), `parse_voice_command`/`VoiceCmd::{Status,Set}`, `maybe_auto_tts(msg, final_text)`, `CommandContext{cfg,state,tex,subagent_depth}` consistent across tasks.
