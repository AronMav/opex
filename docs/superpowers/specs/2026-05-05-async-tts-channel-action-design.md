# Async TTS Channel Action — Design

**Date:** 2026-05-05  
**Status:** Approved  
**Root cause:** [debug session](../../../../.planning/debug/arty-voice-telegram-timeout.md)

## Problem

`execute_yaml_channel_action` runs synchronously inside the 180 s session deadline.  
TTS synthesis on Raspberry Pi takes 90–130 s for a long news digest.  
`resp.bytes().await` in `execute_binary` has no timeout — it waits for the full audio body.  
Result: global `request_timeout_secs` (180 s) fires before synthesis completes.

```
channel_ws: handle_with_status [deadline: 180s]
  └─ pipeline: execute_yaml_channel_action
       └─ tool.execute_binary → resp.bytes().await   ← no per-body timeout
            Qwen3-TTS on Pi: ~130s for long text
       ← timeout fires here
```

## Solution

Move TTS synthesis + channel send into a background task (`BackgroundTtsTask`).  
Agent returns immediately. The session deadline is no longer relevant.

## Architecture

### New module

`crates/hydeclaw-core/src/agent/pipeline/tts_background.rs`

### BackgroundTtsTask

```rust
pub struct BackgroundTtsTask {
    tool:           YamlToolDef,
    args:           serde_json::Value,
    ca:             ChannelActionConfig,
    http_client:    reqwest::Client,
    resolver:       SecretsEnvResolver,
    oauth_ctx:      Option<OAuthContext>,
    channel_router: Option<ChannelActionRouter>,
    ui_event_tx:    Option<broadcast::Sender<String>>,
    bg_tasks:       Arc<TaskTracker>,
    workspace_dir:  PathBuf,
    db:             PgPool,
    upload_key:     Vec<u8>,
    ttl_secs:       u64,
    tool_headers:   Vec<(String, String)>,
    context:        serde_json::Value,   // contains chat_id when in channel session
    agent_name:     String,
}
```

All fields are cheaply cloneable (`Arc` / `Clone`). No borrows — safe to `tokio::spawn`.

### Public API

```rust
impl BackgroundTtsTask {
    /// Construct from CommandContext, cloning required fields.
    pub fn from_ctx(
        ctx: &CommandContext<'_>,
        tool: &YamlToolDef,
        args: &serde_json::Value,
        ca: &ChannelActionConfig,
    ) -> Self;

    /// Spawn the task via bg_tasks (TaskTracker).
    /// Returns the immediate reply string for the agent.
    /// Channel vs UI is detected from self.context["chat_id"].
    pub fn spawn(self) -> &'static str;

    /// Task body — private.
    async fn run(self);
}
```

### Change to existing code

`execute_yaml_channel_action` in [channel_actions.rs](../../../crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs) — **only this function changes**. Caller signature, YAML tools, and dispatch code are untouched.

```rust
pub async fn execute_yaml_channel_action(
    ctx: &CommandContext<'_>,
    tool: &YamlToolDef,
    args: &serde_json::Value,
    ca: &ChannelActionConfig,
) -> String {
    // existing: build resolver, tool_headers (unchanged)
    let task = BackgroundTtsTask::from_ctx(ctx, tool, args, ca);
    let is_channel = args.get("_context")
        .and_then(|c| c.get("chat_id")).is_some();
    task.spawn(is_channel).to_string()
}
```

## Data Flow

### BackgroundTtsTask::run()

```
1. tokio::time::timeout(600s, tool.execute_binary(...))
        ↓ Ok(bytes)                     ↓ Err(e)
        
2. save_binary_to_uploads(workspace_dir, bytes, "audio", upload_key, ttl_secs)
        ↓ (url, media_type)             ↓ Err → handle_error(e)

3a. has_channel_context == true  →  channel path
3b. has_channel_context == false →  ui path
```

### Channel path (Telegram / Discord)

```
channel_router.send(ChannelAction {
    name: "send_voice",
    params: { audio_base64: base64(bytes) },
    context,   // chat_id, etc.
})
→ Ok  →  log INFO, done
→ Err →  channel_router.send(ChannelAction {
              name: "send_message",
              params: { text: "❌ Не удалось отправить голосовое: {e}" },
          })
```

### UI path (no chat_id)

```
notify(db, ui_event_tx, "tts_ready",
       "Аудио готово",
       "{agent_name}",
       json!({ "url": url, "mediaType": media_type }))
→ bell notification with inline <audio> player

on any error:
notify(db, ui_event_tx, "tts_error",
       "Не удалось синтезировать аудио",
       "{agent_name}",
       json!({ "error": e.to_string() }))
```

### Immediate agent reply

| Session type | Returned string |
|---|---|
| Channel (Telegram) | `"🎙 Голосовое синтезируется и будет отправлено в чат. Продолжай разговор."` |
| UI | `"🎙 Аудио синтезируется. Файл появится в уведомлениях через ~1–2 мин."` |

## Graceful Shutdown

`BackgroundTtsTask::spawn()` uses `ctx.state.bg_tasks.spawn(...)` — the existing `TaskTracker`.  
Shutdown calls `bg_tasks.wait_drain(timeout)` — already wired in `AgentState::wait_drain()`.  
No new shutdown logic needed.

## Background Task Timeout

`execute_binary` is wrapped in `tokio::time::timeout(600s)` inside `run()`.  
This replaces the accidental 180 s deadline.  
600 s is generous enough for very long TTS on Pi (worst case observed: ~130 s for a 5-minute digest).

## UI Changes

Single change in the notifications component:

```tsx
// In NotificationRow (or equivalent)
if (notification.type === "tts_ready" && notification.data?.url) {
  return (
    <div>
      <p>{notification.title}</p>
      <audio controls src={notification.data.url} className="w-full mt-2" />
    </div>
  )
}
```

### New notification types

| type | trigger | data |
|---|---|---|
| `tts_ready` | successful background synthesis in UI session | `{ url, mediaType }` |
| `tts_error` | synthesis failure in UI session | `{ error }` |

Channel session errors go to the chat directly — no bell notification needed.

## Files to Create / Modify

| File | Change |
|---|---|
| `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs` | **create** — BackgroundTtsTask |
| `crates/hydeclaw-core/src/agent/pipeline/mod.rs` | add `pub mod tts_background` |
| `crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs` | replace body of `execute_yaml_channel_action` |
| `ui/src/` (NotificationRow or notifications component) | add `tts_ready` / `tts_error` rendering |
| `ui/src/types/api.ts` | add `tts_ready` / `tts_error` to notification type union (if typed) |

## Out of Scope

- `send_photo` channel action — keep synchronous (photos are small, no timeout issue)
- Retry logic for failed TTS — future phase
- Progress indicator in Telegram while synthesizing — future phase
