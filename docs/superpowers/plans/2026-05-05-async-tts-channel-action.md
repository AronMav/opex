# Async TTS Channel Action Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move TTS synthesis + Telegram send out of the synchronous SSE session pipeline into a tracked background task so the 180 s session deadline can't kill a long synthesis.

**Architecture:** New `BackgroundTtsTask` struct in `tts_background.rs` owns all cloneable context it needs (reqwest client, channel router, secrets resolver, etc.). `execute_yaml_channel_action` builds the struct and calls `.spawn()`, returning immediately. The task's `run()` calls toolgate, then branches: channel sessions send audio directly; UI sessions save to uploads and create a `tts_ready` notification.

**Tech Stack:** Rust/tokio, tokio_util::task::TaskTracker, wiremock 0.6 (tests), Next.js/React (frontend)

**Spec:** `docs/superpowers/specs/2026-05-05-async-tts-channel-action-design.md`

---

## File Map

| File | Action |
|---|---|
| `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs` | **create** — BackgroundTtsTask |
| `crates/hydeclaw-core/src/agent/pipeline/mod.rs` | modify — `pub mod tts_background` |
| `crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs` | modify — replace `execute_yaml_channel_action` body |
| `ui/src/components/notification-bell.tsx` | modify — add `tts_ready` / `tts_error` rendering |

---

### Task 1: Scaffold `tts_background.rs`

**Files:**
- Create: `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs`
- Modify: `crates/hydeclaw-core/src/agent/pipeline/mod.rs`

- [ ] **Step 1: Add module declaration to mod.rs**

In `crates/hydeclaw-core/src/agent/pipeline/mod.rs`, add after the existing `pub mod channel_actions;` line:

```rust
pub mod tts_background;
```

- [ ] **Step 2: Create the struct file**

Create `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs`:

```rust
//! Background TTS task — synthesise audio and deliver it outside the
//! SSE session deadline so a slow Qwen3-TTS on Pi can't time out the agent.

use std::sync::Arc;

use base64::Engine as _;
use tokio::sync::broadcast;
use tokio_util::task::TaskTracker;

use crate::agent::channel_actions::{ChannelAction, ChannelActionRouter};
use crate::agent::engine::SecretsEnvResolver;
use crate::tools::yaml_tools::{ChannelActionConfig, OAuthContext, YamlToolDef};

/// Owns everything a background TTS job needs — no borrows, safe to `tokio::spawn`.
pub struct BackgroundTtsTask {
    pub(crate) tool:           YamlToolDef,
    pub(crate) args:           serde_json::Value,
    pub(crate) ca:             ChannelActionConfig,
    pub(crate) http_client:    reqwest::Client,
    /// None only in tests where the YAML tool has no env-var templates.
    pub(crate) resolver:       Option<SecretsEnvResolver>,
    pub(crate) oauth_ctx:      Option<OAuthContext>,
    pub(crate) channel_router: Option<ChannelActionRouter>,
    pub(crate) ui_event_tx:    Option<broadcast::Sender<String>>,
    pub(crate) bg_tasks:       Arc<TaskTracker>,
    pub(crate) workspace_dir:  String,
    pub(crate) db:             sqlx::PgPool,
    pub(crate) upload_key:     [u8; 32],
    pub(crate) ttl_secs:       u64,
    pub(crate) tool_headers:   Vec<(String, String)>,
    pub(crate) context:        serde_json::Value,
    pub(crate) agent_name:     String,
}
```

- [ ] **Step 3: Verify it compiles**

```
make check
```

Expected: no errors (struct exists, no impl yet — that's fine at this stage).

- [ ] **Step 4: Commit scaffold**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/tts_background.rs \
        crates/hydeclaw-core/src/agent/pipeline/mod.rs
git commit -m "feat(tts): scaffold BackgroundTtsTask struct"
```

---

### Task 2: Tests — channel-success path

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs` (add `#[cfg(test)]` module)

- [ ] **Step 1: Add test helpers and channel-success test**

Append to `tts_background.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;
    use tokio_util::task::TaskTracker;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path}};

    /// Lazy PgPool that never connects — safe as long as the test path
    /// doesn't call notify() (UI-path only).
    fn fake_db() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("lazy connect cannot fail")
    }

    /// Build a minimal YamlToolDef pointing at `endpoint`.
    /// No auth / env-var templates → resolver: None is safe.
    fn make_tool(endpoint: &str) -> YamlToolDef {
        serde_yaml::from_str(&format!(
            "name: synthesize_speech\nendpoint: \"{endpoint}\"\nmethod: POST\ntimeout: 10\n"
        ))
        .expect("valid yaml")
    }

    fn make_task(
        server_url: &str,
        router: Option<ChannelActionRouter>,
        context: serde_json::Value,
    ) -> BackgroundTtsTask {
        let (ui_tx, _) = broadcast::channel(4);
        BackgroundTtsTask {
            tool:           make_tool(&format!("{server_url}/v1/audio/speech")),
            args:           serde_json::json!({ "input": "test", "_context": context }),
            ca:             ChannelActionConfig { action: "send_voice".into(), data_field: "_binary".into() },
            http_client:    reqwest::Client::new(),
            // None is valid: execute_binary accepts Option<&dyn EnvResolver>,
            // and our test tool has no env-var templates.
            resolver:       None,
            oauth_ctx:      None,
            channel_router: router,
            ui_event_tx:    Some(ui_tx),
            bg_tasks:       Arc::new(TaskTracker::new()),
            workspace_dir:  std::env::temp_dir().to_string_lossy().into_owned(),
            db:             fake_db(),
            upload_key:     [0u8; 32],
            ttl_secs:       3600,
            tool_headers:   vec![],
            context:        context.clone(),
            agent_name:     "Arty".into(),
        }
    }

    #[tokio::test]
    async fn channel_success_sends_voice_action() {
        // Arrange: fake toolgate returns 8 bytes of audio
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fakewav!"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;

        let context = serde_json::json!({ "chat_id": 42, "channel": "telegram" });
        let task = make_task(&server.uri(), Some(router), context);

        // Act
        task.run().await;

        // Assert: send_voice action was dispatched
        let action = rx.try_recv().expect("send_voice action must arrive");
        assert_eq!(action.name, "send_voice");
        assert!(
            action.params.get("audio_base64").is_some(),
            "params must contain audio_base64"
        );
        // Confirm the reply channel won't cause a panic — send Ok(())
        let _ = action.reply.send(Ok(()));
    }
}
```

- [ ] **Step 2: Run to confirm failure (run() not yet implemented)**

```
cargo test -p hydeclaw-core tts_background -- --nocapture 2>&1 | head -30
```

Expected: compile error — `run()` method does not exist.

---

### Task 3: Implement `run()` — channel path

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs`

- [ ] **Step 1: Add `run()` with channel path**

After the struct definition (before `#[cfg(test)]`) add:

```rust
impl BackgroundTtsTask {
    /// Synthesise audio and deliver it. Called inside `bg_tasks.spawn(...)`.
    pub async fn run(self) {
        let has_channel = self.context.get("chat_id").is_some();

        // ── 1. Synthesise ─────────────────────────────────────────────────────
        let resolver_ref = self.resolver.as_ref().map(|r| r as &dyn crate::tools::yaml_tools::EnvResolver);
        let bytes = match tokio::time::timeout(
            std::time::Duration::from_secs(600),
            self.tool.execute_binary(
                &self.args,
                &self.http_client,
                resolver_ref,
                self.oauth_ctx.as_ref(),
                &self.tool_headers,
            ),
        )
        .await
        {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                tracing::warn!(tool = %self.tool.name, error = %e, "background TTS synthesis failed");
                self.handle_error(&format!("TTS synthesis failed: {e}"), has_channel).await;
                return;
            }
            Err(_) => {
                tracing::warn!(tool = %self.tool.name, "background TTS timed out after 600s");
                self.handle_error("TTS synthesis timed out after 600s", has_channel).await;
                return;
            }
        };

        tracing::info!(tool = %self.tool.name, bytes = bytes.len(), "background TTS synthesis complete");

        // ── 2. Deliver ────────────────────────────────────────────────────────
        if has_channel {
            self.deliver_to_channel(bytes).await;
        } else {
            self.deliver_to_ui(bytes).await;
        }
    }

    /// Send audio to the channel adapter (Telegram / Discord).
    async fn deliver_to_channel(self, bytes: Vec<u8>) {
        let router = match self.channel_router {
            Some(r) => r,
            None => {
                tracing::warn!(
                    agent = %self.agent_name,
                    "background TTS: chat_id present but channel_router is None — dropping"
                );
                return;
            }
        };

        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let param_key = match self.ca.action.as_str() {
            "send_photo" => "image_base64",
            "send_voice" => "audio_base64",
            _            => "data_base64",
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

        if router
            .send(ChannelAction {
                name: self.ca.action.clone(),
                params: serde_json::json!({ param_key: audio_b64 }),
                context: self.context.clone(),
                reply: reply_tx,
                target_channel: None,
            })
            .await
            .is_err()
        {
            tracing::warn!(agent = %self.agent_name, "background TTS: channel router closed before send_voice");
            return;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx).await {
            Ok(Ok(Ok(()))) => {
                tracing::info!(agent = %self.agent_name, "background TTS: send_voice delivered");
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(agent = %self.agent_name, error = %e, "background TTS: send_voice failed");
                self.send_error_message_to_channel(&router,
                    &format!("❌ Не удалось отправить голосовое: {e}")).await;
            }
            Ok(Err(_)) => {
                tracing::warn!(agent = %self.agent_name, "background TTS: send_voice reply dropped");
            }
            Err(_) => {
                tracing::warn!(agent = %self.agent_name, "background TTS: send_voice timed out (60s)");
                self.send_error_message_to_channel(&router,
                    "❌ Отправка голосового в Telegram истекла по таймауту (60s)").await;
            }
        }
    }

    /// Save to uploads and create a UI notification.
    async fn deliver_to_ui(self, bytes: Vec<u8>) {
        use crate::agent::pipeline::handlers::save_binary_to_uploads;
        use crate::gateway::handlers::notifications::notify;

        let (url, media_type) = match save_binary_to_uploads(
            &self.workspace_dir,
            &bytes,
            "audio",
            &self.upload_key,
            self.ttl_secs,
        )
        .await
        {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(agent = %self.agent_name, error = %e, "background TTS: save_to_uploads failed");
                if let Some(tx) = self.ui_event_tx.as_ref() {
                    let _ = notify(
                        &self.db,
                        tx,
                        "tts_error",
                        "Не удалось синтезировать аудио",
                        &format!("Ошибка агента {}: {e}", self.agent_name),
                        serde_json::json!({ "error": e.to_string() }),
                    )
                    .await;
                }
                return;
            }
        };

        if let Some(tx) = self.ui_event_tx.as_ref() {
            let _ = notify(
                &self.db,
                tx,
                "tts_ready",
                "Аудио готово",
                &format!("Синтезировано агентом {}", self.agent_name),
                serde_json::json!({ "url": url, "mediaType": media_type }),
            )
            .await;
        }
    }

    /// Send an error text message back to the channel.
    async fn send_error_message_to_channel(&self, router: &ChannelActionRouter, text: &str) {
        let (reply_tx, _) = tokio::sync::oneshot::channel();
        let _ = router
            .send(ChannelAction {
                name: "send_message".into(),
                params: serde_json::json!({ "text": text }),
                context: self.context.clone(),
                reply: reply_tx,
                target_channel: None,
            })
            .await;
    }

    /// Dispatch error either to channel or log only (no UI notify — requires DB).
    async fn handle_error(&self, msg: &str, has_channel: bool) {
        if has_channel {
            if let Some(ref router) = self.channel_router {
                self.send_error_message_to_channel(
                    router,
                    &format!("❌ Не удалось отправить голосовое: {msg}"),
                )
                .await;
            }
        }
        // UI error path: leave for handle_error callers that have bytes context
        // (deliver_to_ui handles its own errors with notify).
    }
}
```

- [ ] **Step 2: Run channel-success test**

```
cargo test -p hydeclaw-core tts_background::tests::channel_success_sends_voice_action -- --nocapture
```

Expected: `test tts_background::tests::channel_success_sends_voice_action ... ok`

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/tts_background.rs
git commit -m "feat(tts): implement BackgroundTtsTask::run — channel path"
```

---

### Task 4: Tests — error paths

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs`

- [ ] **Step 1: Add error-path tests**

Inside the `#[cfg(test)]` `mod tests` block, append:

```rust
    #[tokio::test]
    async fn channel_router_none_does_not_panic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fakewav!"))
            .mount(&server)
            .await;

        let context = serde_json::json!({ "chat_id": 42 });
        // router = None even though chat_id is present
        let task = make_task(&server.uri(), None, context);
        // Must not panic
        task.run().await;
    }

    #[tokio::test]
    async fn tts_error_sends_message_to_channel() {
        // Arrange: toolgate returns 500 → synthesis error
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let context = serde_json::json!({ "chat_id": 42, "channel": "telegram" });
        let task = make_task(&server.uri(), Some(router), context);

        task.run().await;

        // Assert: error message sent to channel
        let action = rx.try_recv().expect("error send_message must arrive");
        assert_eq!(action.name, "send_message");
        let text = action.params["text"].as_str().unwrap_or("");
        assert!(text.contains('❌'), "error text must start with ❌, got: {text}");
        let _ = action.reply.send(Ok(()));
    }

    #[tokio::test]
    async fn ui_session_does_not_panic() {
        // Arrange: no chat_id → UI path; toolgate returns audio bytes
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fakewav!"))
            .mount(&server)
            .await;

        // No chat_id → deliver_to_ui path
        let context = serde_json::json!({});
        let task = make_task(&server.uri(), None, context);

        // Act — save_binary_to_uploads writes to temp dir; notify() fails silently
        // (lazy DB never connects). Must complete without panic.
        task.run().await;
    }
```

- [ ] **Step 2: Run error-path tests**

```
cargo test -p hydeclaw-core tts_background -- --nocapture
```

Expected: all 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/tts_background.rs
git commit -m "test(tts): add channel error-path tests for BackgroundTtsTask"
```

---

### Task 5: Implement `from_ctx` and `spawn`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/tts_background.rs`

- [ ] **Step 1: Add `from_ctx` and `spawn` to the impl block**

Add these methods to the `impl BackgroundTtsTask` block, before `run()`:

```rust
    /// Construct from the current pipeline context — clones all Arc/cheap fields.
    pub fn from_ctx(
        ctx: &super::CommandContext<'_>,
        tool: &YamlToolDef,
        args: &serde_json::Value,
        ca: &ChannelActionConfig,
    ) -> Self {
        use crate::agent::pipeline::channel_actions::{make_resolver, make_oauth_context};

        let mut tool_headers: Vec<(String, String)> = Vec::new();
        if ca.action == "send_voice" {
            if let Some(prov) = ctx.cfg.agent.tts_provider.as_deref() {
                if !prov.is_empty() {
                    tool_headers.push(("X-Hydeclaw-Provider".into(), prov.into()));
                }
            }
        }
        let context = args.get("_context").cloned().unwrap_or(serde_json::Value::Null);

        Self {
            tool:           tool.clone(),
            args:           args.clone(),
            ca:             ca.clone(),
            http_client:    ctx.tex.http_client.clone(),
            resolver:       Some(make_resolver(&ctx.tex.secrets, &ctx.cfg.agent.name)),
            oauth_ctx:      make_oauth_context(ctx.tex.oauth.as_ref(), &ctx.cfg.agent.name),
            channel_router: ctx.state.channel_router.clone(),
            ui_event_tx:    ctx.state.ui_event_tx.clone(),
            bg_tasks:       ctx.state.bg_tasks.clone(),
            workspace_dir:  ctx.cfg.workspace_dir.clone(),
            db:             ctx.cfg.db.clone(),
            upload_key:     ctx.tex.secrets.get_upload_hmac_key(),
            ttl_secs:       ctx.cfg.app_config.uploads.signed_url_ttl_secs,
            tool_headers,
            context,
            agent_name:     ctx.cfg.agent.name.clone(),
        }
    }

    /// Spawn the task into `bg_tasks` (TaskTracker) and return the immediate
    /// agent reply string. Channel vs UI detected from `self.context["chat_id"]`.
    pub fn spawn(self) -> &'static str {
        let has_channel = self.context.get("chat_id").is_some();
        self.bg_tasks.clone().spawn(async move { self.run().await });
        if has_channel {
            "🎙 Голосовое синтезируется и будет отправлено в чат. Продолжай разговор."
        } else {
            "🎙 Аудио синтезируется. Файл появится в уведомлениях через ~1–2 мин."
        }
    }
```

- [ ] **Step 2: Check it compiles**

```
make check
```

Expected: no errors. If the compiler complains about `workspace_dir` type, verify `ctx.cfg.workspace_dir` — if it's `PathBuf`, change the field to `PathBuf` and call `.to_string_lossy().into_owned()` in `save_binary_to_uploads` call site.

- [ ] **Step 3: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/tts_background.rs
git commit -m "feat(tts): add from_ctx and spawn to BackgroundTtsTask"
```

---

### Task 6: Wire into `execute_yaml_channel_action`

**Files:**
- Modify: `crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs`

- [ ] **Step 1: Replace the body of `execute_yaml_channel_action`**

Open `channel_actions.rs`. Find `execute_yaml_channel_action` (currently ~100 lines, lines 112–230). Replace the entire function body with:

```rust
pub async fn execute_yaml_channel_action(
    ctx: &CommandContext<'_>,
    tool: &crate::tools::yaml_tools::YamlToolDef,
    args: &serde_json::Value,
    ca: &crate::tools::yaml_tools::ChannelActionConfig,
) -> String {
    let task = crate::agent::pipeline::tts_background::BackgroundTtsTask::from_ctx(ctx, tool, args, ca);
    task.spawn().to_string()
}
```

Keep all imports at the top of the file that are still needed (`make_resolver`, `make_oauth_context` are now used by `tts_background.rs` — ensure they remain `pub(crate)` if they were before).

- [ ] **Step 2: Clean up now-unused imports**

Run:
```
make check 2>&1 | grep "unused import\|warning"
```

Remove any imports in `channel_actions.rs` that are now unused (e.g. `base64`, `ph::save_binary_to_uploads`). Keep `make_resolver`, `make_oauth_context`, and `send_channel_message`.

- [ ] **Step 3: Run all pipeline tests**

```
cargo test -p hydeclaw-core -- --nocapture 2>&1 | tail -20
```

Expected: all existing tests pass, the 3 new tts_background tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/agent/pipeline/channel_actions.rs
git commit -m "feat(tts): wire BackgroundTtsTask into execute_yaml_channel_action"
```

---

### Task 7: Frontend — `tts_ready` audio player in notification bell

**Files:**
- Modify: `ui/src/components/notification-bell.tsx`

- [ ] **Step 1: Write a failing test**

Create `ui/src/__tests__/notification-tts.test.tsx`:

```tsx
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import React from "react";

// Minimal stub for the notification item renderer we're about to add
// Import will fail until we export the component
import { TtsNotificationBody } from "@/components/notification-bell";

describe("TtsNotificationBody", () => {
  it("renders an audio player for tts_ready with url", () => {
    render(
      <TtsNotificationBody
        notification={{
          id: "1",
          type: "tts_ready",
          title: "Аудио готово",
          body: "Синтезировано агентом Arty",
          data: { url: "/uploads/test.wav", mediaType: "audio/wav" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />
    );
    const audio = screen.getByTestId("tts-audio-player");
    expect(audio).toBeTruthy();
    expect(audio.getAttribute("src")).toBe("/uploads/test.wav");
  });

  it("renders error text for tts_error", () => {
    render(
      <TtsNotificationBody
        notification={{
          id: "2",
          type: "tts_error",
          title: "Не удалось синтезировать аудио",
          body: "Ошибка агента Arty: connection refused",
          data: { error: "connection refused" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />
    );
    expect(screen.getByText(/connection refused/)).toBeTruthy();
  });
});
```

- [ ] **Step 2: Run to confirm failure**

```bash
cd ui && npm test -- notification-tts 2>&1 | tail -20
```

Expected: compile/import error — `TtsNotificationBody` not exported yet.

- [ ] **Step 3: Add `TtsNotificationBody` export to `notification-bell.tsx`**

In `notification-bell.tsx`, add before the `NotificationBell` function:

```tsx
import type { NotificationRow } from "@/types/api";

// ── TTS notification body ─────────────────────────────────────────────────────

interface TtsNotificationBodyProps {
  notification: NotificationRow;
}

export function TtsNotificationBody({ notification }: TtsNotificationBodyProps) {
  const { type, body, data } = notification;

  if (type === "tts_ready" && data?.url) {
    return (
      <div className="flex flex-col gap-1 w-full" onClick={(e) => e.stopPropagation()}>
        <span className="text-xs text-muted-foreground">{body}</span>
        <audio
          controls
          src={data.url as string}
          className="w-full mt-1 h-8"
          data-testid="tts-audio-player"
        />
      </div>
    );
  }

  if (type === "tts_error") {
    return (
      <span className="text-xs text-destructive line-clamp-2">{body}</span>
    );
  }

  return <span className="text-xs text-muted-foreground line-clamp-2">{body}</span>;
}
```

- [ ] **Step 4: Use `TtsNotificationBody` inside the notification list**

In `notification-bell.tsx`, find the notification list map (around line 136). Replace the body span:

```tsx
// Before:
<span className="text-xs text-muted-foreground line-clamp-2">
  {n.body}
</span>

// After:
<TtsNotificationBody notification={n} />
```

Also update `getNotificationRoute` to return `null` for `tts_ready`/`tts_error`, and update the onClick handler to skip navigation in that case:

```tsx
function getNotificationRoute(type: string): string | null {
  switch (type) {
    case "access_request":  return "/access";
    case "tool_approval":   return "/monitor/?tab=approvals";
    case "agent_error":     return "/monitor/?tab=logs";
    case "watchdog_alert":  return "/monitor/?tab=watchdog";
    case "tts_ready":       return null;  // audio player inline — no navigation
    case "tts_error":       return null;
    default:                return "/monitor/";
  }
}
```

Update the notification onClick to skip `router.push` when route is null:

```tsx
// Before:
onClick={() => {
  if (!n.read) markRead.mutate(n.id);
  router.push(getNotificationRoute(n.type));
}}

// After:
onClick={() => {
  if (!n.read) markRead.mutate(n.id);
  const route = getNotificationRoute(n.type);
  if (route) router.push(route);
}}
```

- [ ] **Step 5: Run tests**

```bash
cd ui && npm test -- notification-tts 2>&1 | tail -20
```

Expected: both tests pass.

- [ ] **Step 6: Build UI to catch TypeScript errors**

```bash
cd ui && npm run build 2>&1 | tail -20
```

Expected: build succeeds with no type errors.

- [ ] **Step 7: Commit**

```bash
git add ui/src/components/notification-bell.tsx \
        ui/src/__tests__/notification-tts.test.tsx
git commit -m "feat(ui): add tts_ready audio player and tts_error in notification bell"
```

---

## Self-Review Checklist

After completing all tasks, verify:

- [ ] `make check` passes (no Rust compile errors or warnings about unused imports)
- [ ] `cargo test -p hydeclaw-core tts_background -- --nocapture` — all 3 tests green
- [ ] `cd ui && npm test` — notification-tts tests pass
- [ ] `cd ui && npm run build` — no TypeScript errors
- [ ] Manually: trigger a TTS call via Telegram; agent responds immediately; voice arrives separately
- [ ] Manually: trigger TTS in UI; agent responds with "аудио синтезируется"; bell notification appears with audio player
