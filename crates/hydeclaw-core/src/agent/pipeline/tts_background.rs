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

impl BackgroundTtsTask {
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

    /// Spawn the task into `bg_tasks` (TaskTracker) and return the tool result
    /// string. The voice/audio is delivered out-of-band by the background task,
    /// so the result is a hidden system instruction telling the LLM to end its
    /// turn silently — no preamble like "voice sent" is wanted in the chat.
    pub fn spawn(self) -> &'static str {
        let has_channel = self.context.get("chat_id").is_some();
        self.bg_tasks.clone().spawn(async move { self.run().await });
        if has_channel {
            "[SYSTEM] Audio dispatched in background; the user will receive a voice message directly. \
             Do NOT mention voice, audio, or synthesis in your reply. \
             Do NOT write acknowledgements like \"voice sent\" or \"sending now\". \
             End your turn immediately with no further text."
        } else {
            "[SYSTEM] Audio dispatched in background; will appear in the UI notifications panel as an audio player. \
             Do NOT mention voice, audio, or synthesis in your reply. \
             End your turn immediately with no further text."
        }
    }

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
        // Destructure to avoid partial-move borrow issues when router is consumed.
        let BackgroundTtsTask {
            ca,
            context,
            agent_name,
            channel_router,
            ..
        } = self;

        let router = match channel_router {
            Some(r) => r,
            None => {
                tracing::warn!(
                    agent = %agent_name,
                    "background TTS: chat_id present but channel_router is None — dropping"
                );
                return;
            }
        };

        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let param_key = match ca.action.as_str() {
            "send_photo" => "image_base64",
            "send_voice" => "audio_base64",
            _            => "data_base64",
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

        if router
            .send(ChannelAction {
                name: ca.action.clone(),
                params: serde_json::json!({ param_key: audio_b64 }),
                context: context.clone(),
                reply: reply_tx,
                target_channel: None,
            })
            .await
            .is_err()
        {
            tracing::warn!(agent = %agent_name, "background TTS: channel router closed before send_voice");
            return;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx).await {
            Ok(Ok(Ok(()))) => {
                tracing::info!(agent = %agent_name, "background TTS: send_voice delivered");
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(agent = %agent_name, error = %e, "background TTS: send_voice failed");
                send_error_to_channel(&router, &context,
                    &format!("❌ Не удалось отправить голосовое: {e}")).await;
            }
            Ok(Err(_)) => {
                tracing::warn!(agent = %agent_name, "background TTS: send_voice reply dropped");
            }
            Err(_) => {
                tracing::warn!(agent = %agent_name, "background TTS: send_voice timed out (60s)");
                send_error_to_channel(&router, &context,
                    "❌ Отправка голосового в Telegram истекла по таймауту (60s)").await;
            }
        }
    }

    /// Save to uploads and create a UI notification.
    async fn deliver_to_ui(self, bytes: Vec<u8>) {
        use crate::agent::pipeline::handlers::save_binary_to_uploads;
        use crate::gateway::notify;

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

    /// Dispatch error either to channel or log only (no UI notify — requires DB).
    async fn handle_error(&self, msg: &str, has_channel: bool) {
        if has_channel {
            if let Some(ref router) = self.channel_router {
                send_error_to_channel(
                    router,
                    &self.context,
                    &format!("❌ Не удалось отправить голосовое: {msg}"),
                )
                .await;
            }
        }
        // UI error path is intentionally absent here: synthesis errors arrive
        // before any bytes exist, and notify() requires DB access. deliver_to_ui()
        // owns the UI error path and calls notify() locally with bytes context.
    }
}

/// Send a text error message back to the channel (free fn to avoid partial-move issues).
async fn send_error_to_channel(router: &ChannelActionRouter, context: &serde_json::Value, text: &str) {
    let (reply_tx, _) = tokio::sync::oneshot::channel();
    let _ = router
        .send(ChannelAction {
            name: "send_message".into(),
            params: serde_json::json!({ "text": text }),
            context: context.clone(),
            reply: reply_tx,
            target_channel: None,
        })
        .await;
}

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
            "name: synthesize_speech\ndescription: test TTS tool\nendpoint: \"{endpoint}\"\nmethod: POST\ntimeout: 10\n"
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

        // Act: run the task in the background; concurrently drain the reply so
        // deliver_to_channel doesn't block on the 60s reply timeout.
        let run_handle = tokio::spawn(task.run());

        // Give the task time to synthesise and dispatch the action, then reply.
        let action = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                loop {
                    if let Ok(a) = rx.try_recv() { return a; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await
        .expect("send_voice action must arrive within 10s");

        // Assert: send_voice action was dispatched
        assert_eq!(action.name, "send_voice");
        assert!(
            action.params.get("audio_base64").is_some(),
            "params must contain audio_base64"
        );
        // Confirm the reply channel — send Ok(()) so deliver_to_channel can finish.
        let _ = action.reply.send(Ok(()));

        // Wait for run() to complete cleanly.
        run_handle.await.expect("task should complete without panic");
    }

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
        // Arrange: toolgate returns 400 (non-retryable) → synthesis error
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid request"))
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
        assert!(text.contains('❌'), "error text must contain ❌, got: {text}");
        // The error message from handle_error wraps the synthesis failure detail,
        // which for a non-retryable HTTP 400 includes "HTTP 400" from yaml_tools.
        assert!(
            text.contains("Не удалось отправить голосовое") && text.contains("HTTP 400"),
            "error text must include the wrapper phrase and the underlying HTTP failure, got: {text}"
        );
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
}
