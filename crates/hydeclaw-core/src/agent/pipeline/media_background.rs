//! Background media task — synthesise / generate media (TTS audio, image,
//! video) and deliver it outside the SSE session deadline so a slow generator
//! (e.g. Qwen3-TTS or FLUX on Pi) cannot time out the agent.
//!
//! Originally `tts_background.rs`. Renamed because this code is invoked for
//! any YAML tool with a binary `channel_action` (`send_voice`, `send_photo`,
//! `send_video`), not only TTS — keeping the TTS-specific name caused the
//! `generate_image` tool to return an "Audio dispatched..." system message
//! to the LLM.

use std::sync::Arc;

use base64::Engine as _;
use tokio::sync::broadcast;
use tokio_util::task::TaskTracker;

use crate::agent::channel_actions::{ChannelAction, ChannelActionRouter};
use crate::agent::engine::SecretsEnvResolver;
use crate::tools::yaml_tools::{ChannelActionConfig, OAuthContext, YamlToolDef};

// ── MediaKind ────────────────────────────────────────────────────────────────

/// Classifies a YAML-tool `channel_action` into a media flavour. Drives the
/// system text returned to the LLM, the channel-router payload key, and the
/// `save_binary_to_uploads` hint. All per-action behaviour lives here so a
/// new media kind only needs one match arm per concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Voice,
    Photo,
    Video,
    Other,
}

impl MediaKind {
    /// Map a `channel_action` string (e.g. `"send_photo"`) to a kind.
    /// Unknown actions fall back to [`MediaKind::Other`] so adapters that
    /// invent their own action names still get sane defaults.
    pub fn from_action(action: &str) -> Self {
        match action {
            "send_voice" => Self::Voice,
            "send_photo" => Self::Photo,
            "send_video" => Self::Video,
            _ => Self::Other,
        }
    }

    /// Capitalised noun used as the subject of the LLM system message
    /// (e.g. `"Audio dispatched..."`, `"Image dispatched..."`).
    fn subject(self) -> &'static str {
        match self {
            Self::Voice => "Audio",
            Self::Photo => "Image",
            Self::Video => "Video",
            Self::Other => "Media",
        }
    }

    /// Lowercase media noun used inside the channel-style instruction
    /// (e.g. `"voice message"`, `"photo message"`).
    fn channel_noun(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Photo => "photo",
            Self::Video => "video",
            Self::Other => "media",
        }
    }

    /// Phrase listing words the LLM must NOT use in its reply. Aligned with
    /// the actual media kind so the model doesn't get told "do not mention
    /// audio" while we just sent an image.
    fn forbidden_words(self) -> &'static str {
        match self {
            Self::Voice => "voice, audio, or synthesis",
            Self::Photo => "image, picture, or generation",
            Self::Video => "video or generation",
            Self::Other => "media generation",
        }
    }

    /// Channel router payload key that channel adapters expect for this kind.
    pub fn channel_param_key(self) -> &'static str {
        match self {
            Self::Voice => "audio_base64",
            Self::Photo => "image_base64",
            Self::Video => "video_base64",
            Self::Other => "data_base64",
        }
    }

    /// Hint passed to `save_binary_to_uploads` so it can pick the right
    /// extension/MIME when magic bytes don't disambiguate.
    pub fn upload_hint(self) -> &'static str {
        match self {
            Self::Voice => "audio",
            Self::Photo => "image",
            Self::Video => "video",
            Self::Other => "binary",
        }
    }

    /// UI notification event type for the success path. Voice keeps the
    /// historical `"tts_ready"` for UI backward-compat (audio player handler);
    /// other kinds use kind-specific events that the UI bell renders inline.
    pub fn notification_ready_event(self) -> &'static str {
        match self {
            Self::Voice => "tts_ready",
            Self::Photo => "image_ready",
            Self::Video => "video_ready",
            Self::Other => "media_ready",
        }
    }

    /// UI notification event type for the error path.
    pub fn notification_error_event(self) -> &'static str {
        match self {
            Self::Voice => "tts_error",
            Self::Photo => "image_error",
            Self::Video => "video_error",
            Self::Other => "media_error",
        }
    }

    /// Localised (ru) title for the success-path UI notification.
    pub fn notification_ready_title(self) -> &'static str {
        match self {
            Self::Voice => "Аудио готово",
            Self::Photo => "Изображение готово",
            Self::Video => "Видео готово",
            Self::Other => "Медиа готово",
        }
    }

    /// Localised (ru) title for the error-path UI notification.
    pub fn notification_error_title(self) -> &'static str {
        match self {
            Self::Voice => "Не удалось синтезировать аудио",
            Self::Photo => "Не удалось сгенерировать изображение",
            Self::Video => "Не удалось сгенерировать видео",
            Self::Other => "Не удалось подготовить медиа",
        }
    }

    /// Build the system instruction returned to the LLM after the background
    /// task is spawned. The actual media is delivered out-of-band, so this
    /// string tells the LLM to end its turn quietly without any preamble.
    ///
    /// `has_channel = true` — media goes to a chat (Telegram/Discord/etc.);
    /// `has_channel = false` — media is saved to uploads + UI notification.
    pub fn system_message(self, has_channel: bool) -> String {
        let subj = self.subject();
        let forbid = self.forbidden_words();
        let dest = if has_channel {
            format!(
                "the user will receive a {} message directly",
                self.channel_noun()
            )
        } else {
            format!(
                "will appear in the UI notifications panel as a {} attachment",
                subj.to_lowercase()
            )
        };
        format!(
            "[SYSTEM] {subj} dispatched in background; {dest}. \
             Do NOT mention {forbid} in your reply. \
             Do NOT write acknowledgements like \"sent\" or \"sending now\". \
             End your turn immediately with no further text."
        )
    }
}

// ── Per-kind routing ─────────────────────────────────────────────────────────

/// Resolve the per-agent provider override header for this media kind.
/// Returns `Some(("X-Hydeclaw-Provider", value))` when the agent has a
/// non-empty override for the kind's capability; `None` otherwise.
///
/// Each kind reads only its own provider field — Voice never reads
/// `imagegen_provider` and Photo never reads `tts_provider`. Keeping this
/// as a free function (not tied to `CommandContext`) lets us unit-test the
/// cross-contamination guard without a full engine setup.
pub fn provider_header_for(
    kind: MediaKind,
    tts_provider: Option<&str>,
    imagegen_provider: Option<&str>,
) -> Option<(String, String)> {
    let prov = match kind {
        MediaKind::Voice => tts_provider,
        MediaKind::Photo => imagegen_provider,
        MediaKind::Video | MediaKind::Other => None,
    };
    prov.filter(|s| !s.is_empty())
        .map(|p| ("X-Hydeclaw-Provider".to_string(), p.to_string()))
}

// ── BackgroundMediaTask ──────────────────────────────────────────────────────

/// Owns everything a background media job needs — no borrows, safe to `tokio::spawn`.
pub struct BackgroundMediaTask {
    pub(crate) tool:           YamlToolDef,
    pub(crate) args:           serde_json::Value,
    pub(crate) ca:             ChannelActionConfig,
    pub(crate) kind:           MediaKind,
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

impl BackgroundMediaTask {
    /// Construct from the current pipeline context — clones all Arc/cheap fields.
    pub fn from_ctx(
        ctx: &super::CommandContext<'_>,
        tool: &YamlToolDef,
        args: &serde_json::Value,
        ca: &ChannelActionConfig,
    ) -> Self {
        use crate::agent::pipeline::channel_actions::{make_oauth_context, make_resolver};

        let kind = MediaKind::from_action(&ca.action);

        // Per-kind routing headers — see `provider_header_for` for the policy.
        let mut tool_headers: Vec<(String, String)> = Vec::new();
        if let Some(header) = provider_header_for(
            kind,
            ctx.cfg.agent.tts_provider.as_deref(),
            ctx.cfg.agent.imagegen_provider.as_deref(),
        ) {
            tool_headers.push(header);
        }

        let context = args.get("_context").cloned().unwrap_or(serde_json::Value::Null);

        // Background media bypasses the SSE deadline, but the YAML tool's own
        // per-tool timeout (default 60s) wraps `builder.send()`. Toolgate
        // (FastAPI) buffers the response until the full payload is ready, so
        // headers don't arrive until generation is complete — which can take
        // 90–130s for TTS or longer for image generation on a Raspberry Pi.
        // Override to 600s here so background tasks aren't killed by the
        // per-tool timeout. See:
        // crates/hydeclaw-core/src/tools/yaml_tools.rs send_request.
        let mut bg_tool = tool.clone();
        if bg_tool.timeout < 600 {
            bg_tool.timeout = 600;
        }

        // The shared engine http_client has a 120s timeout
        // (gateway/handlers/agents/lifecycle.rs). reqwest aborts the request
        // at that deadline regardless of our outer tokio timeout — surfaces
        // as "HTTP request failed". Build a dedicated long-timeout client
        // here so a long news digest or high-quality image render can finish.
        let bg_http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .unwrap_or_else(|_| ctx.tex.http_client.clone());

        Self {
            tool:           bg_tool,
            args:           args.clone(),
            ca:             ca.clone(),
            kind,
            http_client:    bg_http_client,
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

    /// Spawn the task into `bg_tasks` (TaskTracker) and return the LLM-facing
    /// system instruction. The media is delivered out-of-band, so the result
    /// is a hidden system instruction telling the LLM to end its turn
    /// silently — no "voice sent" / "image sent" preamble in the chat.
    pub fn spawn(self) -> String {
        let has_channel = self.context.get("chat_id").is_some();
        let kind = self.kind;
        self.bg_tasks.clone().spawn(async move { self.run().await });
        kind.system_message(has_channel)
    }

    /// Generate the media bytes and deliver them. Called inside `bg_tasks.spawn(...)`.
    pub async fn run(self) {
        let has_channel = self.context.get("chat_id").is_some();
        let kind = self.kind;

        // ── 1. Generate / synthesise ──────────────────────────────────────────
        let resolver_ref = self
            .resolver
            .as_ref()
            .map(|r| r as &dyn crate::tools::yaml_tools::EnvResolver);
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
                tracing::warn!(tool = %self.tool.name, kind = ?kind, error = %e, "background media generation failed");
                self.handle_error(&format!("media generation failed: {e}"), has_channel).await;
                return;
            }
            Err(_) => {
                tracing::warn!(tool = %self.tool.name, kind = ?kind, "background media generation timed out after 600s");
                self.handle_error("media generation timed out after 600s", has_channel).await;
                return;
            }
        };

        tracing::info!(tool = %self.tool.name, kind = ?kind, bytes = bytes.len(), "background media generation complete");

        // ── 2. Deliver ────────────────────────────────────────────────────────
        if has_channel {
            self.deliver_to_channel(bytes).await;
        } else {
            self.deliver_to_ui(bytes, kind).await;
        }
    }

    /// Send media bytes to the channel adapter (Telegram / Discord / ...).
    async fn deliver_to_channel(self, bytes: Vec<u8>) {
        // Destructure to avoid partial-move borrow issues when router is consumed.
        let BackgroundMediaTask {
            ca,
            kind,
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
                    action = %ca.action,
                    "background media: chat_id present but channel_router is None — dropping"
                );
                return;
            }
        };

        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let param_key = kind.channel_param_key();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

        if router
            .send(ChannelAction {
                name: ca.action.clone(),
                params: serde_json::json!({ param_key: payload_b64 }),
                context: context.clone(),
                reply: reply_tx,
                target_channel: None,
            })
            .await
            .is_err()
        {
            tracing::warn!(
                agent = %agent_name,
                action = %ca.action,
                "background media: channel router closed before send"
            );
            return;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx).await {
            Ok(Ok(Ok(()))) => {
                tracing::info!(agent = %agent_name, action = %ca.action, "background media: delivered");
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(
                    agent = %agent_name, action = %ca.action, error = %e,
                    "background media: delivery failed"
                );
                send_error_to_channel(
                    &router,
                    &context,
                    &format!("❌ Не удалось отправить медиа ({}): {e}", kind.channel_noun()),
                )
                .await;
            }
            Ok(Err(_)) => {
                tracing::warn!(
                    agent = %agent_name, action = %ca.action,
                    "background media: reply dropped"
                );
            }
            Err(_) => {
                tracing::warn!(
                    agent = %agent_name, action = %ca.action,
                    "background media: delivery timed out (60s)"
                );
                send_error_to_channel(
                    &router,
                    &context,
                    &format!(
                        "❌ Отправка медиа ({}) в канал истекла по таймауту (60s)",
                        kind.channel_noun()
                    ),
                )
                .await;
            }
        }
    }

    /// Save to uploads and create a UI notification.
    ///
    /// All media kinds emit a notification via kind-specific event types
    /// ([`MediaKind::notification_ready_event`] /
    /// [`MediaKind::notification_error_event`]). Voice retains
    /// `"tts_ready"`/`"tts_error"` for backward compatibility with the
    /// existing UI audio-player handler; Photo/Video/Other use dedicated
    /// events that the UI notification bell renders inline (image preview,
    /// video player, etc.).
    async fn deliver_to_ui(self, bytes: Vec<u8>, kind: MediaKind) {
        use crate::agent::pipeline::handlers::save_binary_to_uploads;
        use crate::gateway::notify;

        let (url, media_type) = match save_binary_to_uploads(
            &self.workspace_dir,
            &bytes,
            kind.upload_hint(),
            &self.upload_key,
            self.ttl_secs,
        )
        .await
        {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(
                    agent = %self.agent_name, kind = ?kind, error = %e,
                    "background media: save_to_uploads failed"
                );
                if let Some(tx) = self.ui_event_tx.as_ref() {
                    let _ = notify(
                        &self.db,
                        tx,
                        kind.notification_error_event(),
                        kind.notification_error_title(),
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
                kind.notification_ready_event(),
                kind.notification_ready_title(),
                &format!("Подготовлено агентом {}", self.agent_name),
                serde_json::json!({ "url": url, "mediaType": media_type }),
            )
            .await;
        }
    }

    /// Dispatch error either to channel or log only (no UI notify — requires DB).
    async fn handle_error(&self, msg: &str, has_channel: bool) {
        if has_channel
            && let Some(ref router) = self.channel_router
        {
            send_error_to_channel(
                router,
                &self.context,
                &format!(
                    "❌ Не удалось отправить медиа ({}): {msg}",
                    self.kind.channel_noun()
                ),
            )
            .await;
        }
        // UI error path is intentionally absent here: generation errors arrive
        // before any bytes exist, and notify() requires DB access. deliver_to_ui()
        // owns the UI error path and calls notify() locally (Voice only).
    }
}

/// Send a text error message back to the channel (free fn to avoid partial-move issues).
async fn send_error_to_channel(
    router: &ChannelActionRouter,
    context: &serde_json::Value,
    text: &str,
) {
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
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    // ── MediaKind unit tests (no I/O) ────────────────────────────────────────

    #[test]
    fn from_action_classifies_known_actions() {
        assert_eq!(MediaKind::from_action("send_voice"), MediaKind::Voice);
        assert_eq!(MediaKind::from_action("send_photo"), MediaKind::Photo);
        assert_eq!(MediaKind::from_action("send_video"), MediaKind::Video);
    }

    #[test]
    fn from_action_unknown_falls_back_to_other() {
        assert_eq!(MediaKind::from_action("send_sticker"), MediaKind::Other);
        assert_eq!(MediaKind::from_action(""), MediaKind::Other);
    }

    #[test]
    fn channel_param_key_matches_legacy_voice_key() {
        // Channel adapters historically expect these exact keys; do not rename.
        assert_eq!(MediaKind::Voice.channel_param_key(), "audio_base64");
        assert_eq!(MediaKind::Photo.channel_param_key(), "image_base64");
        assert_eq!(MediaKind::Video.channel_param_key(), "video_base64");
        assert_eq!(MediaKind::Other.channel_param_key(), "data_base64");
    }

    #[test]
    fn notification_event_voice_keeps_legacy_tts_names() {
        // UI handler in notification-bell.tsx keys off these exact strings.
        assert_eq!(MediaKind::Voice.notification_ready_event(), "tts_ready");
        assert_eq!(MediaKind::Voice.notification_error_event(), "tts_error");
    }

    // ── provider_header_for ─────────────────────────────────────────────────
    //
    // Pure function — no CommandContext / async / I/O. Drives the per-agent
    // routing header that `from_ctx` injects on `tool_headers`. Covers the
    // critical regression: must NOT cross provider override fields between
    // kinds (Voice→tts_provider, Photo→imagegen_provider).

    #[test]
    fn provider_header_voice_with_tts_provider_returns_header() {
        let h = provider_header_for(MediaKind::Voice, Some("nova-cloud"), None);
        assert_eq!(h, Some(("X-Hydeclaw-Provider".into(), "nova-cloud".into())));
    }

    #[test]
    fn provider_header_photo_with_imagegen_provider_returns_header() {
        let h = provider_header_for(MediaKind::Photo, None, Some("flux-pro"));
        assert_eq!(h, Some(("X-Hydeclaw-Provider".into(), "flux-pro".into())));
    }

    #[test]
    fn provider_header_voice_without_tts_provider_returns_none() {
        assert_eq!(provider_header_for(MediaKind::Voice, None, Some("ignored")), None);
    }

    #[test]
    fn provider_header_voice_with_empty_tts_provider_returns_none() {
        assert_eq!(provider_header_for(MediaKind::Voice, Some(""), None), None);
    }

    #[test]
    fn provider_header_photo_without_imagegen_provider_returns_none() {
        assert_eq!(provider_header_for(MediaKind::Photo, Some("ignored"), None), None);
    }

    #[test]
    fn provider_header_photo_with_empty_imagegen_provider_returns_none() {
        assert_eq!(provider_header_for(MediaKind::Photo, None, Some("")), None);
    }

    #[test]
    fn provider_header_voice_does_not_use_imagegen_provider() {
        // Cross-contamination guard: a Voice action must never read imagegen_provider.
        let h = provider_header_for(MediaKind::Voice, None, Some("flux-pro"));
        assert_eq!(h, None, "Voice must not pick up imagegen_provider");
    }

    #[test]
    fn provider_header_photo_does_not_use_tts_provider() {
        // Cross-contamination guard: a Photo action must never read tts_provider.
        let h = provider_header_for(MediaKind::Photo, Some("nova-cloud"), None);
        assert_eq!(h, None, "Photo must not pick up tts_provider");
    }

    #[test]
    fn provider_header_video_returns_none_even_with_both_set() {
        let h = provider_header_for(MediaKind::Video, Some("nova"), Some("flux"));
        assert_eq!(h, None, "Video has no provider override yet");
    }

    #[test]
    fn provider_header_other_returns_none_even_with_both_set() {
        let h = provider_header_for(MediaKind::Other, Some("nova"), Some("flux"));
        assert_eq!(h, None, "Other has no provider override");
    }

    // ── upload_hint per kind (regression guard) ─────────────────────────────

    #[test]
    fn upload_hint_per_kind() {
        // Drives save_binary_to_uploads() extension/MIME picking when magic
        // bytes are ambiguous — wrong hint means an image saved as .ogg.
        assert_eq!(MediaKind::Voice.upload_hint(), "audio");
        assert_eq!(MediaKind::Photo.upload_hint(), "image");
        assert_eq!(MediaKind::Video.upload_hint(), "video");
        assert_eq!(MediaKind::Other.upload_hint(), "binary");
    }

    // ── notification titles (ru, content checks) ────────────────────────────

    #[test]
    fn notification_ready_title_distinct_and_kind_appropriate() {
        assert_eq!(MediaKind::Voice.notification_ready_title(), "Аудио готово");
        assert_eq!(MediaKind::Photo.notification_ready_title(), "Изображение готово");
        assert_eq!(MediaKind::Video.notification_ready_title(), "Видео готово");
        assert_eq!(MediaKind::Other.notification_ready_title(), "Медиа готово");
    }

    #[test]
    fn notification_error_title_distinct_and_kind_appropriate() {
        // Wording must match the actual kind — so a photo failure does not
        // tell the user "не удалось синтезировать аудио".
        assert!(
            MediaKind::Voice.notification_error_title().contains("аудио"),
            "voice error title must mention аудио, got: {}",
            MediaKind::Voice.notification_error_title()
        );
        assert!(
            MediaKind::Photo.notification_error_title().contains("изображение"),
            "photo error title must mention изображение, got: {}",
            MediaKind::Photo.notification_error_title()
        );
        assert!(
            MediaKind::Video.notification_error_title().contains("видео"),
            "video error title must mention видео, got: {}",
            MediaKind::Video.notification_error_title()
        );
        assert!(
            MediaKind::Other.notification_error_title().contains("медиа"),
            "other error title must mention медиа, got: {}",
            MediaKind::Other.notification_error_title()
        );
    }

    #[test]
    fn notification_event_per_kind_distinct() {
        let kinds = [MediaKind::Voice, MediaKind::Photo, MediaKind::Video, MediaKind::Other];
        let ready: Vec<&'static str> = kinds.iter().map(|k| k.notification_ready_event()).collect();
        let error: Vec<&'static str> = kinds.iter().map(|k| k.notification_error_event()).collect();
        assert_eq!(ready, vec!["tts_ready", "image_ready", "video_ready", "media_ready"]);
        assert_eq!(error, vec!["tts_error", "image_error", "video_error", "media_error"]);
    }

    #[test]
    fn system_message_for_voice_mentions_audio_not_image() {
        let msg = MediaKind::Voice.system_message(true);
        assert!(msg.contains("Audio dispatched"), "voice msg must say Audio: {msg}");
        assert!(msg.contains("voice, audio, or synthesis"), "voice msg must forbid audio words: {msg}");
        assert!(!msg.contains("Image dispatched"), "voice msg must NOT mention image: {msg}");
    }

    #[test]
    fn system_message_for_photo_mentions_image_not_audio() {
        // Regression test for the original bug: generate_image used to return
        // "Audio dispatched..." which made the LLM stay silent about the picture.
        let msg = MediaKind::Photo.system_message(true);
        assert!(msg.contains("Image dispatched"), "photo msg must say Image: {msg}");
        assert!(
            msg.contains("image, picture, or generation"),
            "photo msg must forbid image words: {msg}"
        );
        assert!(!msg.contains("Audio dispatched"), "photo msg must NOT say Audio: {msg}");
        assert!(!msg.contains("voice"), "photo msg must NOT mention voice: {msg}");
    }

    #[test]
    fn system_message_for_video_mentions_video_not_audio() {
        let msg = MediaKind::Video.system_message(true);
        assert!(msg.contains("Video dispatched"), "video msg must say Video: {msg}");
        assert!(msg.contains("video or generation"), "video msg must forbid video words: {msg}");
        assert!(!msg.contains("Audio dispatched"), "video msg must NOT say Audio: {msg}");
    }

    #[test]
    fn system_message_for_other_falls_back_to_media() {
        let msg = MediaKind::Other.system_message(true);
        assert!(msg.contains("Media dispatched"), "other msg must say Media: {msg}");
        assert!(!msg.contains("Audio"), "other msg must NOT say Audio: {msg}");
        assert!(!msg.contains("Image"), "other msg must NOT say Image: {msg}");
    }

    #[test]
    fn system_message_channel_vs_ui_path_differs() {
        let chat = MediaKind::Photo.system_message(true);
        let ui   = MediaKind::Photo.system_message(false);
        assert!(chat.contains("user will receive"), "chat path mentions delivery to user: {chat}");
        assert!(ui.contains("UI notifications panel"), "ui path mentions UI panel: {ui}");
    }

    #[test]
    fn system_message_always_ends_turn_quietly() {
        // The "do not reply" instruction is the whole point — every kind must include it.
        for kind in [MediaKind::Voice, MediaKind::Photo, MediaKind::Video, MediaKind::Other] {
            for has_channel in [true, false] {
                let msg = kind.system_message(has_channel);
                assert!(
                    msg.contains("End your turn immediately"),
                    "kind={kind:?} has_channel={has_channel} must instruct LLM to end turn: {msg}"
                );
            }
        }
    }

    // ── Integration-style tests (wiremock for execute_binary) ────────────────

    /// Lazy PgPool that never connects — safe as long as the test path
    /// doesn't call notify() (UI-path only for Voice would need a real DB).
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
            "name: test_media\ndescription: test media tool\nendpoint: \"{endpoint}\"\nmethod: POST\ntimeout: 10\n"
        ))
        .expect("valid yaml")
    }

    fn make_task(
        server_url: &str,
        action: &str,
        router: Option<ChannelActionRouter>,
        context: serde_json::Value,
    ) -> BackgroundMediaTask {
        let (ui_tx, _) = broadcast::channel(4);
        let ca = ChannelActionConfig {
            action: action.to_string(),
            data_field: "_binary".into(),
        };
        let kind = MediaKind::from_action(&ca.action);
        BackgroundMediaTask {
            tool:           make_tool(&format!("{server_url}/v1/audio/speech")),
            args:           serde_json::json!({ "input": "test", "_context": context }),
            ca,
            kind,
            http_client:    reqwest::Client::new(),
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
    async fn voice_channel_success_sends_voice_action_with_audio_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fakewav!"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;

        let context = serde_json::json!({ "chat_id": 42, "channel": "telegram" });
        let task = make_task(&server.uri(), "send_voice", Some(router), context);

        let run_handle = tokio::spawn(task.run());

        let action = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Ok(a) = rx.try_recv() {
                    return a;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("send_voice action must arrive within 10s");

        assert_eq!(action.name, "send_voice");
        assert!(
            action.params.get("audio_base64").is_some(),
            "params must contain audio_base64 (legacy key for voice)"
        );
        let _ = action.reply.send(Ok(()));
        run_handle.await.expect("task should complete without panic");
    }

    #[tokio::test]
    async fn photo_channel_success_sends_photo_action_with_image_key() {
        // Regression test for the generate_image bug — image must use
        // image_base64 payload key, not audio_base64.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;

        let context = serde_json::json!({ "chat_id": 42, "channel": "telegram" });
        let task = make_task(&server.uri(), "send_photo", Some(router), context);

        let run_handle = tokio::spawn(task.run());

        let action = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Ok(a) = rx.try_recv() {
                    return a;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("send_photo action must arrive within 10s");

        assert_eq!(action.name, "send_photo");
        assert!(
            action.params.get("image_base64").is_some(),
            "photo action must use image_base64, got: {}",
            action.params
        );
        assert!(
            action.params.get("audio_base64").is_none(),
            "photo action must NOT use audio_base64: {}",
            action.params
        );
        let _ = action.reply.send(Ok(()));
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
        let task = make_task(&server.uri(), "send_voice", None, context);
        // Must not panic
        task.run().await;
    }

    #[tokio::test]
    async fn error_sends_message_to_channel_for_any_kind() {
        // Generation error path — uses the kind-aware error message.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid request"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let context = serde_json::json!({ "chat_id": 42, "channel": "telegram" });
        let task = make_task(&server.uri(), "send_photo", Some(router), context);

        task.run().await;

        let action = rx.try_recv().expect("error send_message must arrive");
        assert_eq!(action.name, "send_message");
        let text = action.params["text"].as_str().unwrap_or("");
        assert!(text.contains('❌'), "error text must contain ❌, got: {text}");
        // For send_photo we expect the photo channel noun, not "voice".
        assert!(
            text.contains("photo"),
            "error text for send_photo must mention photo, got: {text}"
        );
        assert!(
            text.contains("HTTP 400"),
            "error text must include underlying HTTP failure, got: {text}"
        );
        let _ = action.reply.send(Ok(()));
    }

    #[tokio::test]
    async fn ui_session_does_not_panic_for_voice() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fakewav!"))
            .mount(&server)
            .await;

        let context = serde_json::json!({});
        let task = make_task(&server.uri(), "send_voice", None, context);

        // notify() fails silently (lazy DB never connects). Must complete without panic.
        task.run().await;
    }

    // ── deliver_to_ui notify event-type wiring (#[sqlx::test], needs DB) ──
    //
    // Closes the gap between MediaKind::notification_*_event() / *_title()
    // (covered by unit tests) and the actual `notify(...)` call inside
    // deliver_to_ui. Without these, swapping `kind.notification_ready_event()`
    // for a hardcoded "tts_ready" in deliver_to_ui would not be caught.
    //
    // Requires DATABASE_URL — runs under `make test-db`. Skipped silently
    // by `cargo test` without DB (matches the existing 8 sqlx::test gates
    // documented in CLAUDE.md).

    fn make_task_with_db(
        server_url: &str,
        action: &str,
        db: sqlx::PgPool,
        context: serde_json::Value,
    ) -> BackgroundMediaTask {
        let (ui_tx, _) = broadcast::channel(4);
        let ca = ChannelActionConfig {
            action: action.to_string(),
            data_field: "_binary".into(),
        };
        let kind = MediaKind::from_action(&ca.action);
        BackgroundMediaTask {
            tool:           make_tool(&format!("{server_url}/v1/audio/speech")),
            args:           serde_json::json!({ "input": "test", "_context": context }),
            ca,
            kind,
            http_client:    reqwest::Client::new(),
            resolver:       None,
            oauth_ctx:      None,
            channel_router: None,
            ui_event_tx:    Some(ui_tx),
            bg_tasks:       Arc::new(TaskTracker::new()),
            workspace_dir:  std::env::temp_dir().to_string_lossy().into_owned(),
            db,
            upload_key:     [0u8; 32],
            ttl_secs:       3600,
            tool_headers:   vec![],
            context:        context.clone(),
            agent_name:     "Arty".into(),
        }
    }

    async fn assert_notification_inserted(
        pool: &sqlx::PgPool,
        expected_type: &str,
        expected_title: &str,
    ) {
        // notify() emits exactly one row per call; we own this DB and inserted nothing else.
        let (ty, title): (String, String) = sqlx::query_as(
            "SELECT type, title FROM notifications ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(pool)
        .await
        .expect("notifications row must exist");
        assert_eq!(ty, expected_type, "notification.type mismatch");
        assert_eq!(title, expected_title, "notification.title mismatch");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_ui_voice_emits_tts_ready(pool: sqlx::PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"OggS\x00\x00fakeaudio"))
            .mount(&server)
            .await;
        let task = make_task_with_db(&server.uri(), "send_voice", pool.clone(), serde_json::json!({}));
        task.run().await;
        assert_notification_inserted(&pool, "tts_ready", "Аудио готово").await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_ui_photo_emits_image_ready(pool: sqlx::PgPool) {
        // Regression test for the original generate_image bug: image MUST emit
        // image_ready, not tts_ready (which would render an audio player around
        // a PNG in the UI bell).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG\r\n\x1a\nfake"))
            .mount(&server)
            .await;
        let task = make_task_with_db(&server.uri(), "send_photo", pool.clone(), serde_json::json!({}));
        task.run().await;
        assert_notification_inserted(&pool, "image_ready", "Изображение готово").await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_ui_video_emits_video_ready(pool: sqlx::PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x00\x00\x00 ftypisomfake"))
            .mount(&server)
            .await;
        let task = make_task_with_db(&server.uri(), "send_video", pool.clone(), serde_json::json!({}));
        task.run().await;
        assert_notification_inserted(&pool, "video_ready", "Видео готово").await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_ui_other_emits_media_ready(pool: sqlx::PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"arbitrary-bytes"))
            .mount(&server)
            .await;
        let task = make_task_with_db(&server.uri(), "send_sticker", pool.clone(), serde_json::json!({}));
        task.run().await;
        assert_notification_inserted(&pool, "media_ready", "Медиа готово").await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_ui_photo_save_failure_emits_image_error(pool: sqlx::PgPool) {
        // Force save_binary_to_uploads to fail by pointing workspace_dir at a
        // path that cannot be created (a NUL byte is invalid on every OS).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG"))
            .mount(&server)
            .await;
        let mut task = make_task_with_db(
            &server.uri(),
            "send_photo",
            pool.clone(),
            serde_json::json!({}),
        );
        task.workspace_dir = "\0invalid-path".into();
        task.run().await;
        assert_notification_inserted(&pool, "image_error", "Не удалось сгенерировать изображение").await;
    }

    #[tokio::test]
    async fn ui_session_does_not_panic_for_photo() {
        // Photo in UI mode currently skips notify() — must still complete cleanly.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake"))
            .mount(&server)
            .await;

        let context = serde_json::json!({});
        let task = make_task(&server.uri(), "send_photo", None, context);

        task.run().await;
    }
}
