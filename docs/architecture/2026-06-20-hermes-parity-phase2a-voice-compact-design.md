# Hermes-parity Phase 2a — `/voice` toggle + `/compact` in UI — design

- **Date:** 2026-06-20
- **Status:** Approved (design); pending implementation plan
- **Branch:** `feat/hermes-parity-phase2a-voice-compact`
- **Origin:** Hermes gap analysis (see memory `reference_hermes_agent.md`). Phase 2 was split: **2a = `/voice` + `/compact`-UI** (this spec), **2b = `/goal` autonomous loop** (separate, later).

## Context & motivation

Two of the smaller Phase-2 gaps versus Hermes:

| # | Component | Value | Effort | Local-verifiable |
|---|-----------|-------|--------|------------------|
| F | `/voice on\|off\|status` per-chat toggle (auto-TTS replies) | medium | M | partial (Rust logic) |
| G | `/compact` in the chat UI slash menu | small | S | yes (vitest) |

**Out of scope (deferred):** `/voice only` text suppression (voice replaces text — needs deeper channel-sink changes); `/compact` token-delta feedback; the `/goal` loop (Phase 2b).

## Cross-cutting principles

- TDD: tests first (project convention).
- rustls-only, clippy `-D warnings` clean, no `Co-Authored-By` in commits, no push without explicit user request.
- Migrations are runtime-loaded; `make remote-deploy` now syncs them (fixed 2026-06-20).
- Verify Rust logic locally with `cargo test --bin hydeclaw-core`; DB tests with the test postgres; auto-TTS end-to-end on the server (toolgate + Telegram). The TS channel adapter is NOT touched — the auto-TTS hook lives in Rust.

---

## Component G — `/compact` in the chat slash menu

### Problem
The `/compact` slash command is fully implemented in the backend
(`agent/pipeline/commands.rs`) but is absent from the UI autocomplete dropdown
(`ui/src/app/(authenticated)/chat/parts/SlashMenu.tsx`, which lists only
`/new`, `/reset`, `/stop`, `/think:*`). Users must know to type it blindly.

### Design
Add an entry to the `COMMANDS` array in `SlashMenu.tsx`:
`{ cmd: "/compact", key: "chat.slash_compact" }`, and add the `chat.slash_compact`
translation key to `ui/src/i18n/locales/en.json` and `ru.json`.

### Files
- `ui/src/app/(authenticated)/chat/parts/SlashMenu.tsx`
- `ui/src/i18n/locales/en.json`, `ui/src/i18n/locales/ru.json`

### Tests
- vitest: the SlashMenu, filtered by `/comp`, surfaces the `/compact` entry.

### Local-verifiable: ✅ (`cd ui && npm test`)

---

## Component F — `/voice on|off|status` per-chat toggle

### Problem
HydeClaw has one-shot TTS (the agent calls the TTS YAML tool, which emits a
`send_voice` channel action) but no way for a channel user to put a specific chat
into a persistent "read every reply aloud" mode. Hermes has `/voice on|off|tts`
persisted per `(platform, chat_id)`.

### Design — storage decision: **DB-backed** (approved)
A per-chat mode must survive restarts and is naturally keyed by the channel + chat,
so a small table fits HydeClaw's DB-centric model.

**Migration `migrations/055_channel_voice_modes.sql`:**
```sql
CREATE TABLE channel_voice_modes (
    channel  TEXT NOT NULL,   -- channel type/name from IncomingMessage.channel
    chat_id  TEXT NOT NULL,   -- from IncomingMessage.context->>'chat_id'
    mode     TEXT NOT NULL DEFAULT 'off'
             CHECK (mode IN ('off', 'on')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel, chat_id)
);
```
(`on` = auto-TTS each reply in addition to text; `off` = default. The `only`
text-suppression mode is deferred; the CHECK constraint can be widened later.)

**`crates/hydeclaw-core/src/db/channel_voice_modes.rs` (new):**
- `get_voice_mode(db, channel, chat_id) -> Result<String>` (returns `"off"` when absent).
- `set_voice_mode(db, channel, chat_id, mode) -> Result<()>` (upsert).

### Semantics — `/voice` slash command
Handled in `agent/pipeline/commands.rs` (`handle_command`, which already receives
`msg: &IncomingMessage` and a `CommandContext` carrying `db`). The handler reads the
channel + chat directly from the message (`msg.channel`, `msg.context["chat_id"]`) and
writes via `ctx.db` — **no `CommandContext` change is needed**:
- `/voice on` → set mode `on`; reply "Voice replies enabled for this chat."
- `/voice off` → set mode `off`; reply "Voice replies disabled."
- `/voice` or `/voice status` → report current mode.
- No `chat_id` in context (e.g. web UI) → reply that `/voice` only applies to chat channels.

A pure `parse_voice_command(arg) -> VoiceCmd` helper (`Status | Set("on"|"off")`) is
unit-tested without DB.

### Auto-TTS hook
After the pipeline produces the final assistant text for a **channel** turn, if the
chat's mode is `on`, synthesize speech and send it as a voice message — without
touching the transport-agnostic `finalize`:

- Hook location: the channel entry path (`handle_with_status` in
  `agent/engine/run.rs`), **after** `pipeline::execute` returns `outcome` (the final
  text is `outcome.final_text`; channel + chat come from `msg`). This is past the
  slash-command early-exit, so it only fires on real assistant turns.
- Channel router: obtained from `self.state().channel_router`
  (`AgentState.channel_router: Option<ChannelActionRouter>`) — the **same** mechanism the
  TTS YAML tool and approval buttons already use (`router.send(action)`). It is NOT a
  parameter of `handle_with_status`.
- Synthesis + delivery: reuse the existing TTS channel-action path
  (`POST {toolgate_url}/v1/audio/speech` → save to uploads → `send_voice` `ChannelAction`
  via the router) rather than reimplementing it (DRY). Factor the synth+send logic the
  TTS YAML tool uses into a reusable helper if needed.
- Failures are non-fatal: log a warning and continue (never block the text reply).
- Skip when the final text is empty, the chat mode is `off`, or the turn was
  interrupted/failed (`outcome.status` not a successful completion).

### Files
- `migrations/055_channel_voice_modes.sql` (new)
- `crates/hydeclaw-core/src/db/channel_voice_modes.rs` (new) + `db/mod.rs` export
- `crates/hydeclaw-core/src/agent/pipeline/commands.rs` (`/voice` command + `parse_voice_command`)
- `crates/hydeclaw-core/src/agent/engine/run.rs` (`handle_with_status` post-pipeline auto-TTS hook)
- Possibly `crates/hydeclaw-core/src/agent/pipeline/commands.rs` `CommandContext` (add channel + chat_id fields)

### Tests
- Unit (local): `parse_voice_command` maps `on`/`off`/empty/`status` correctly; unknown arg → error.
- DB (test postgres): `set_voice_mode` + `get_voice_mode` round-trip; default `off` when absent; upsert overwrites.
- End-to-end (server): `/voice on` in a Telegram chat, then a normal message → the reply arrives as both text and a voice message; `/voice off` → text only.

### Local-verifiable: ⚠️ Rust logic + DB yes; auto-TTS delivery on the server.

---

## Implementation order
1. **G** (`/compact` menu) — trivial, isolated, locally verified.
2. **F-storage** — migration 055 + `channel_voice_modes` module + DB tests.
3. **F-command** — `parse_voice_command` + `/voice` in `commands.rs` (+ CommandContext channel/chat).
4. **F-hook** — auto-TTS in `handle_with_status`.

## Deploy / verification path
- Local: `cd ui && npm test` (G); `cargo test --bin hydeclaw-core` + test-postgres (F logic + DB).
- Server: `make remote-deploy` (now syncs migration 055) + `make doctor`; smoke `/voice on` → message → expect text + voice; `/voice off` → text only.
