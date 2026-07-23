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
use uuid::Uuid;

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
    ///
    /// No longer called from production code — the "media ready" bell
    /// notification was removed (see [`deliver_to_ui`]/`persist_channel_media_inline`).
    /// Kept for symmetry with [`Self::notification_error_event`] and covered
    /// by unit tests; `#[allow(dead_code)]` because the bin target doesn't
    /// see the `#[cfg(test)]` call sites.
    ///
    /// [`deliver_to_ui`]: BackgroundMediaTask::deliver_to_ui
    #[allow(dead_code)]
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

    /// Build an inline tool result for the web-UI session path so the media
    /// renders directly in the chat stream (via [`engine::FILE_PREFIX`] which
    /// chat-history.ts parses into an inline image / audio / video element)
    /// rather than only landing in the notification bell.
    ///
    /// `file_marker_json` must be a JSON object string with at least `url`
    /// (and ideally `mediaType`), as produced by `save_binary_to_uploads`.
    pub fn inline_tool_result(self, file_marker_json: &str) -> String {
        let url = serde_json::from_str::<serde_json::Value>(file_marker_json)
            .ok()
            .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(String::from))
            .unwrap_or_default();
        format!(
            "{}{}\n[SYSTEM] {} delivered inline in chat.\nURL: {}",
            crate::agent::engine::FILE_PREFIX,
            file_marker_json,
            self.subject(),
            url,
        )
    }

    /// Localised (ru) title for the success-path UI notification.
    ///
    /// No longer called from production code (see
    /// [`Self::notification_ready_event`] doc); kept for symmetry and unit
    /// test coverage.
    #[allow(dead_code)]
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

/// Resolve the ordered provider-chain header sets for this media kind from
/// the profile slots. Returns one header-set per attempt, in chain order:
/// `[("X-Opex-Provider", provider)]`, plus `("X-Opex-Voice", voice)` for
/// [`MediaKind::Voice`] when that chain entry has a non-empty `voice`.
///
/// Each kind reads only its own capability slot — Voice never reads the
/// `imagegen` slot and Photo never reads the `tts` slot. Keeping this as a
/// free function (not tied to `CommandContext`) lets us unit-test the
/// cross-contamination guard without a full engine setup.
/// Same as `provider_attempts_with_override` with no override — the original
/// chain order. Kept for test compatibility.
#[cfg(test)]
pub fn provider_attempts_for(
    kind: MediaKind,
    slots: &crate::db::profiles::Slots,
) -> Vec<Vec<(String, String)>> {
    provider_attempts_with_override(kind, slots, None)
}

/// Same as `provider_attempts_for` but with an optional per-turn provider
/// override `(slot, provider)` set by the `profile` tool's `switch` action.
/// When the override matches this kind's capability slot, the named provider
/// is moved to the front of the attempt chain (other providers follow as
/// fallback). When the override's slot doesn't match this kind, the chain
/// is unchanged.
pub fn provider_attempts_with_override(
    kind: MediaKind,
    slots: &crate::db::profiles::Slots,
    override_provider: Option<(&str, &str)>,
) -> Vec<Vec<(String, String)>> {
    let cap = match kind {
        MediaKind::Voice => "tts",
        MediaKind::Photo => "imagegen",
        MediaKind::Video | MediaKind::Other => return Vec::new(),
    };
    let chain = match slots.get(cap) {
        Some(c) if !c.is_empty() => c,
        _ => return Vec::new(),
    };

    // If an override is set for this exact slot, reorder: the override
    // provider goes first, then the rest in their original order.
    let overridden = override_provider
        .filter(|(slot, _)| *slot == cap)
        .map(|(_, provider)| provider.to_string());

    let mut entries: Vec<&crate::db::profiles::SlotEntry> = chain.iter().collect();
    if let Some(ref preferred) = overridden {
        entries.sort_by_key(|e| e.provider != *preferred);
    }

    entries
        .iter()
        .map(|e| {
            let mut h = vec![("X-Opex-Provider".to_string(), e.provider.clone())];
            if kind == MediaKind::Voice
                && let Some(v) = e.voice.as_deref().filter(|v| !v.is_empty())
            {
                h.push(("X-Opex-Voice".to_string(), v.to_string()));
            }
            h
        })
        .collect()
}

/// Whether a background-media provider failure is worth retrying against the
/// next provider in the profile chain. Network/transport failures (`None` —
/// no HTTP response was ever received) and server-side/rate-limit responses
/// (5xx, 429) are retryable; any other 4xx is treated as a permanent
/// rejection of this request (bad input, auth, etc.) that another provider
/// would also reject, so it is not retried.
pub fn is_provider_retryable(status: Option<reqwest::StatusCode>) -> bool {
    match status {
        None => true,
        Some(s) => s.is_server_error() || s == reqwest::StatusCode::TOO_MANY_REQUESTS,
    }
}

/// Best-effort extraction of the HTTP status code embedded in the error
/// message produced by [`crate::tools::yaml_tools::YamlToolDef::execute_binary`]
/// (`"tool '{name}' returned HTTP {status}: {body}"`). `reqwest::StatusCode`'s
/// `Display` prints the code AND its reason phrase (e.g. `503 Service
/// Unavailable`), so the parser takes ASCII digits until the first non-digit
/// to survive the reason suffix. Transport-level failures (the
/// request never got a response) don't carry this substring and correctly
/// resolve to `None`, which [`is_provider_retryable`] treats as retryable.
fn extract_status_from_error(e: &anyhow::Error) -> Option<reqwest::StatusCode> {
    const MARKER: &str = "returned HTTP ";
    let s = e.to_string();
    let rest = s.split_once(MARKER)?.1;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u16>().ok().and_then(|n| reqwest::StatusCode::from_u16(n).ok())
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
    pub(crate) base_url:       String,
    pub(crate) db:             sqlx::PgPool,
    pub(crate) upload_key:     [u8; 32],
    pub(crate) retention_days: u32,
    /// Ordered header-sets, one per provider-chain attempt (see
    /// `provider_attempts_for`). Empty when the media kind has no profile
    /// slot (Video/Other) — `run()` then falls back to a single attempt
    /// with no extra headers.
    pub(crate) provider_attempts: Vec<Vec<(String, String)>>,
    pub(crate) context:        serde_json::Value,
    pub(crate) agent_name:     String,
    /// Pre-allocated `messages` row id for the persisted tool result. When
    /// `Some(_)`, `deliver_to_channel` (after a successful Telegram send)
    /// prepends a `__file__:{json}\n` marker to that row's `content` so the
    /// UI inline parser renders the channel-delivered media when the session
    /// is reloaded in the web UI. `None` for off-the-record paths (subagent /
    /// openai_compat / cron) — the channel send still happens, the DB
    /// prepend is just skipped.
    ///
    /// Sourced from `_context.tool_message_id`, which is stamped by the
    /// sequential dispatch branch in `pipeline::parallel` (gated on
    /// `persist_ctx.is_some()`).
    pub(crate) tool_message_id: Option<Uuid>,
}

impl BackgroundMediaTask {
    /// Construct from the current pipeline context — clones all Arc/cheap fields.
    /// `override_provider` is the per-turn capability provider override
    /// `(slot, provider)` from the `profile` tool, if any.
    pub fn from_ctx(
        ctx: &super::CommandContext<'_>,
        tool: &YamlToolDef,
        args: &serde_json::Value,
        ca: &ChannelActionConfig,
        override_provider: Option<(&str, &str)>,
    ) -> Option<Self> {
        use crate::agent::pipeline::channel_actions::{make_oauth_context, make_resolver};

        let kind = MediaKind::from_action(&ca.action);

        // Per-kind provider chain — see `provider_attempts_with_override` for the policy.
        let provider_attempts = provider_attempts_with_override(
            kind,
            &ctx.cfg.profile_slots,
            override_provider,
        );

        let context = args.get("_context").cloned().unwrap_or(serde_json::Value::Null);

        // Resolve the persisted tool-message row id from `_context`. The
        // sequential dispatch branch in `pipeline::parallel` stamps this when
        // `persist_ctx.is_some()`. Absent / unparseable ⇒ None (legitimate
        // non-persisting path — subagent / openai_compat / cron).
        let tool_message_id = context
            .get("tool_message_id")
            .and_then(|v| v.as_str())
            .and_then(|s| match Uuid::parse_str(s) {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        raw = %s,
                        "background media: _context.tool_message_id present but not a UUID; treating as absent"
                    );
                    None
                }
            });

        // Background media bypasses the SSE deadline, but the YAML tool's own
        // per-tool timeout (default 60s) wraps `builder.send()`. Toolgate
        // (FastAPI) buffers the response until the full payload is ready, so
        // headers don't arrive until generation is complete — which can take
        // 90–130s for TTS or longer for image generation on a Raspberry Pi.
        // Override to 600s here so background tasks aren't killed by the
        // per-tool timeout. See:
        // crates/opex-core/src/tools/yaml_tools.rs send_request.
        let mut bg_tool = tool.clone();
        if bg_tool.timeout < 600 {
            bg_tool.timeout = 600;
        }

        // The shared engine http_client has a 120s timeout
        // (gateway/handlers/agents/lifecycle.rs). reqwest aborts the request
        // at that deadline regardless of our outer tokio timeout — surfaces
        // as "HTTP request failed". Build a dedicated long-timeout client
        // here so a long news digest or high-quality image render can finish.
        //
        // T01 §3: this used to be a raw `reqwest::Client::builder()` with no
        // SSRF DNS-resolver and the default auto-following redirect policy,
        // regardless of `tool.endpoint` — a channel_action (TTS/imagegen)
        // YAML tool could bypass the SSRF guard entirely. Route through the
        // same is_internal_endpoint gate the regular YAML-tool dispatch path
        // uses (engine_dispatch.rs, handlers::handle_tool_test).
        // Literal-IP SSRF gate: `select_ssrf_aware_client` only DNS-filters, so
        // a literal private/metadata IP in the endpoint would slip through.
        // Refuse to build the delivery task for a blocked endpoint.
        if let Err(e) = crate::net::ssrf::validate_outbound_endpoint(&tool.endpoint) {
            tracing::warn!(tool = %tool.name, endpoint = %tool.endpoint, "channel_action background endpoint blocked by SSRF guard: {e}");
            return None;
        }

        let bg_http_client = crate::net::ssrf::select_ssrf_aware_client(
            &tool.endpoint,
            std::time::Duration::from_secs(600),
        );

        Some(Self {
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
            // Root-relative: the upload URL is only ever rendered in the
            // same-origin web UI (`__file__:` marker), so it must not depend
            // on `gateway.public_url`. See web_uploads_base().
            base_url:       crate::uploads::web_uploads_base().to_string(),
            db:             ctx.cfg.db.clone(),
            upload_key:     ctx.tex.secrets.get_upload_hmac_key(),
            retention_days: ctx.cfg.app_config.cleanup.uploads_retention_days,
            provider_attempts,
            context,
            agent_name:     ctx.cfg.agent.name.clone(),
            tool_message_id,
        })
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

        // ── 1. Generate / synthesise, retrying down the profile provider chain ──
        //
        // Each attempt uses the next provider's header-set (X-Opex-Provider [+
        // X-Opex-Voice]). Media kinds without a profile slot (Video/Other) get
        // a single no-extra-headers attempt. On success we break out with the
        // bytes and fall through to the existing (unduplicated) delivery path
        // below; on a permanent failure — or the chain exhausted — we fall
        // through to the existing (unduplicated) error path.
        let resolver_ref = self
            .resolver
            .as_ref()
            .map(|r| r as &dyn crate::tools::yaml_tools::EnvResolver);
        let attempts: Vec<Vec<(String, String)>> = if self.provider_attempts.is_empty() {
            vec![Vec::new()]
        } else {
            self.provider_attempts.clone()
        };

        let mut generated: Option<Vec<u8>> = None;
        let mut failure: Option<String> = None;

        // Fix 1b: now that a per-provider timeout fails over to the next chain
        // entry (see the `Err(_)` arm below), an N-provider chain of *hung*
        // providers would otherwise compound to N × 600s. Divide the 600s
        // budget across the attempts so the total worst-case stays bounded at
        // ~600s regardless of chain length (single-attempt kinds keep the full
        // 600s). Both the timeout constant and the failover arm live in this
        // same loop, so this cap is fully local.
        let per_attempt_timeout =
            std::time::Duration::from_secs(600 / attempts.len().max(1) as u64);

        for (i, headers) in attempts.iter().enumerate() {
            match tokio::time::timeout(
                per_attempt_timeout,
                self.tool.execute_binary(
                    &self.args,
                    &self.http_client,
                    resolver_ref,
                    self.oauth_ctx.as_ref(),
                    headers,
                ),
            )
            .await
            {
                Ok(Ok(b)) => {
                    generated = Some(b);
                    break;
                }
                Ok(Err(e)) => {
                    let status = extract_status_from_error(&e);
                    if i + 1 < attempts.len() && is_provider_retryable(status) {
                        tracing::warn!(
                            tool = %self.tool.name, kind = ?kind, attempt = i, error = %e,
                            "media provider failed; trying next in profile chain"
                        );
                        continue;
                    }
                    tracing::warn!(tool = %self.tool.name, kind = ?kind, error = %e, "background media generation failed");
                    failure = Some(format!("media generation failed: {e}"));
                    break;
                }
                Err(_) => {
                    // Fix 1: a hung provider (connection accepted, no response)
                    // that trips the outer per-attempt timeout is the most
                    // common real-world dead-provider failure — exactly what a
                    // reserve chain exists to survive. Fail over to the next
                    // chain entry instead of recording terminal failure. Only
                    // the FINAL attempt's timeout is terminal (mirrors the
                    // retryable-error arm above).
                    if i + 1 < attempts.len() {
                        tracing::warn!(
                            tool = %self.tool.name, kind = ?kind, attempt = i,
                            "media provider timed out; trying next in profile chain"
                        );
                        continue;
                    }
                    tracing::warn!(tool = %self.tool.name, kind = ?kind, "background media generation timed out");
                    failure = Some("media generation timed out".to_string());
                    break;
                }
            }
        }

        let bytes = match generated {
            Some(b) => b,
            None => {
                let msg = failure
                    .unwrap_or_else(|| "media generation failed: no provider attempt ran".to_string());
                self.handle_error(&msg, has_channel).await;
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
    ///
    /// On a successful channel send, ALSO save the bytes to the `uploads` DB
    /// table (owner_type='tool_output') and prepend a
    /// `__file__:{url, mediaType}\n` marker to the persisted tool message row
    /// (via [`prepend_message_content`]) so reloading the session in the web
    /// UI renders the media inline. Both additions are best-effort: failures
    /// are logged at `warn!` level and do NOT regress the channel-delivery
    /// promise (the user already received the bytes in Telegram / Discord).
    /// No web-bell notification is emitted for this path — the user already
    /// has the media in-channel.
    ///
    /// On any non-success channel-send arm, NOTHING after the send is
    /// attempted.
    ///
    /// [`prepend_message_content`]: crate::db::sessions::prepend_message_content
    async fn deliver_to_channel(self, bytes: Vec<u8>) {
        // Hold onto everything we need post-send BEFORE destructuring the
        // router out for ownership. Using individual `let` bindings (rather
        // than a struct destructure) keeps the post-send save/update path
        // readable without having to thread fields through a helper.
        let kind = self.kind;
        let agent_name = self.agent_name.clone();
        let action = self.ca.action.clone();
        let context = self.context.clone();
        let base_url = self.base_url.clone();
        let upload_key = self.upload_key;
        let retention_days = self.retention_days;
        let db = self.db.clone();
        let ui_event_tx = self.ui_event_tx.clone();
        let tool_message_id = self.tool_message_id;
        let channel_router = self.channel_router;

        let router = match channel_router {
            Some(r) => r,
            None => {
                tracing::warn!(
                    agent = %agent_name,
                    action = %action,
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
                name: action.clone(),
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
                action = %action,
                "background media: channel router closed before send"
            );
            return;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx).await {
            Ok(Ok(Ok(()))) => {
                tracing::info!(agent = %agent_name, action = %action, "background media: delivered");
                // Channel send succeeded — now mirror the same media into the
                // session's web-UI representation. Failures here do NOT regress
                // the channel-delivery success.
                persist_channel_media_inline(
                    &db,
                    retention_days,
                    &base_url,
                    &bytes,
                    kind,
                    &upload_key,
                    tool_message_id,
                    ui_event_tx.as_ref(),
                    &agent_name,
                )
                .await;
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(
                    agent = %agent_name, action = %action, error = %e,
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
                    agent = %agent_name, action = %action,
                    "background media: reply dropped"
                );
            }
            Err(_) => {
                tracing::warn!(
                    agent = %agent_name, action = %action,
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

    /// Save to uploads. On success, the media is available at the returned
    /// URL but no web-bell notification is emitted (the ready-notify was
    /// removed — the UI no longer pings on background media completion).
    /// On failure, a kind-specific error notification is still emitted via
    /// [`MediaKind::notification_error_event`] so the user learns generation
    /// failed.
    async fn deliver_to_ui(self, bytes: Vec<u8>, kind: MediaKind) {
        use crate::agent::pipeline::handlers::save_binary_to_uploads;
        use crate::gateway::notify;

        let (_url, _media_type) = match save_binary_to_uploads(
            &self.db,
            self.retention_days,
            &bytes,
            kind.upload_hint(),
            &self.upload_key,
            &self.base_url,
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

/// After a successful channel send, mirror the same media into the session's
/// web-UI representation:
///
/// 1. Save the bytes to the `uploads` DB table (owner_type='tool_output')
///    so the UI has a stable id-based URL.
/// 2. Prepend `__file__:{url, mediaType}\n` to the persisted tool message row
///    (only when `tool_message_id` is `Some(_)`) so reloading the session in
///    the web UI renders the media inline. The `chat-history.ts:196` parser
///    keys off `FILE_PREFIX`.
///
/// No web-bell `<kind>_ready` notification is emitted here anymore — the
/// user already received the media in-channel, so a bell ping would be
/// redundant. `ui_event_tx` is accepted but unused (kept for call-site /
/// signature stability).
///
/// Both steps are best-effort and cascade independently: if save fails, the
/// prepend never fires (no URL to point at).
///
/// In every error arm, the channel delivery already happened — failure here
/// must NOT abort the caller, only log a `warn!`.
#[allow(clippy::too_many_arguments)]
async fn persist_channel_media_inline(
    db: &sqlx::PgPool,
    retention_days: u32,
    base_url: &str,
    bytes: &[u8],
    kind: MediaKind,
    upload_key: &[u8; 32],
    tool_message_id: Option<Uuid>,
    _ui_event_tx: Option<&broadcast::Sender<String>>,
    agent_name: &str,
) {
    use crate::agent::pipeline::handlers::save_binary_to_uploads;

    let (url, media_type) = match save_binary_to_uploads(
        db,
        retention_days,
        bytes,
        kind.upload_hint(),
        upload_key,
        base_url,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(
                agent = %agent_name, kind = ?kind, error = %e,
                "background media: post-channel save_to_uploads failed; skipping inline mirror"
            );
            return;
        }
    };

    if let Some(id) = tool_message_id {
        let marker_json = serde_json::json!({"url": &url, "mediaType": &media_type}).to_string();
        let prefix = format!("{}{marker_json}\n", crate::agent::engine::FILE_PREFIX);
        if let Err(e) = crate::db::sessions::prepend_message_content(db, id, &prefix).await {
            // Don't return — the bell ping is still useful even if the inline
            // marker didn't land on the persisted row.
            tracing::warn!(
                agent = %agent_name, kind = ?kind, msg_id = %id, error = %e,
                "background media: prepend_message_content failed; bell ping will still fire"
            );
        }
    } else {
        tracing::debug!(
            agent = %agent_name, kind = ?kind,
            "background media: tool_message_id absent; skipping inline DB prepend"
        );
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
        matchers::{header, method, path},
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

    // ── provider_attempts_for ────────────────────────────────────────────────
    //
    // Pure function — no CommandContext / async / I/O. Drives the profile
    // provider chain that `from_ctx` injects on `provider_attempts`. Covers
    // the critical regression: must NOT cross capability slots between kinds
    // (Voice→tts slot, Photo→imagegen slot).

    #[test]
    fn attempts_voice_chain_with_voice_header() {
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert(
            "tts".into(),
            vec![
                crate::db::profiles::SlotEntry {
                    provider: "mm".into(),
                    model: None,
                    voice: Some("Champ".into()),
                },
                crate::db::profiles::SlotEntry { provider: "silero".into(), model: None, voice: None },
            ],
        );
        let a = provider_attempts_for(MediaKind::Voice, &slots);
        assert_eq!(a.len(), 2);
        assert!(a[0].contains(&("X-Opex-Provider".into(), "mm".into())));
        assert!(a[0].contains(&("X-Opex-Voice".into(), "Champ".into())));
        assert_eq!(a[1], vec![("X-Opex-Provider".to_string(), "silero".to_string())]);
    }

    #[test]
    fn attempts_photo_never_reads_tts_slot() {
        // Cross-contamination guard: a Photo action must never read the tts slot.
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert(
            "tts".into(),
            vec![crate::db::profiles::SlotEntry { provider: "mm".into(), model: None, voice: None }],
        );
        assert!(provider_attempts_for(MediaKind::Photo, &slots).is_empty());
    }

    #[test]
    fn attempts_voice_never_reads_imagegen_slot() {
        // Cross-contamination guard: a Voice action must never read the imagegen slot.
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert(
            "imagegen".into(),
            vec![crate::db::profiles::SlotEntry { provider: "flux-pro".into(), model: None, voice: None }],
        );
        assert!(provider_attempts_for(MediaKind::Voice, &slots).is_empty());
    }

    #[test]
    fn attempts_video_and_other_always_empty() {
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert(
            "tts".into(),
            vec![crate::db::profiles::SlotEntry { provider: "mm".into(), model: None, voice: None }],
        );
        slots.insert(
            "imagegen".into(),
            vec![crate::db::profiles::SlotEntry { provider: "flux-pro".into(), model: None, voice: None }],
        );
        assert!(provider_attempts_for(MediaKind::Video, &slots).is_empty());
        assert!(provider_attempts_for(MediaKind::Other, &slots).is_empty());
    }

    #[test]
    fn attempts_empty_slot_returns_empty() {
        let slots = crate::db::profiles::Slots::new();
        assert!(provider_attempts_for(MediaKind::Voice, &slots).is_empty());
        assert!(provider_attempts_for(MediaKind::Photo, &slots).is_empty());
    }

    // ── is_provider_retryable ────────────────────────────────────────────────

    #[test]
    fn retryable_statuses() {
        assert!(is_provider_retryable(None)); // сеть/timeout
        assert!(is_provider_retryable(Some(reqwest::StatusCode::BAD_GATEWAY)));
        assert!(is_provider_retryable(Some(reqwest::StatusCode::SERVICE_UNAVAILABLE)));
        assert!(is_provider_retryable(Some(reqwest::StatusCode::TOO_MANY_REQUESTS))); // 429
        assert!(!is_provider_retryable(Some(reqwest::StatusCode::BAD_REQUEST))); // 4xx
    }

    // ── extract_status_from_error ────────────────────────────────────────────

    #[test]
    fn extract_status_parses_execute_binary_error_format() {
        // Mirror the REAL bail! output: `StatusCode`'s Display prints the code
        // followed by its reason phrase (space-separated), so the parser must
        // survive the `Service Unavailable` suffix and still yield 503.
        let e = anyhow::anyhow!(
            "tool 'synthesize_speech' returned HTTP 503 Service Unavailable: upstream busy"
        );
        assert_eq!(extract_status_from_error(&e), Some(reqwest::StatusCode::SERVICE_UNAVAILABLE));
    }

    #[test]
    fn extract_status_transport_error_returns_none() {
        let e = anyhow::anyhow!("HTTP request failed: error sending request");
        assert_eq!(extract_status_from_error(&e), None);
    }

    // ── inline_tool_result (web UI in-chat delivery) ────────────────────────
    //
    // For UI sessions (no chat_id), media should appear inline in the chat
    // stream — not just as a notification in the bell. The tool result must
    // start with FILE_PREFIX so chat-history.ts:196 picks it up and renders
    // an image/audio/video element in place.

    #[test]
    fn inline_tool_result_starts_with_file_prefix_for_image() {
        let json = r#"{"url":"/uploads/x.png","mediaType":"image/png"}"#;
        let out = MediaKind::Photo.inline_tool_result(json);
        assert!(
            out.starts_with(crate::agent::engine::FILE_PREFIX),
            "inline result must start with __file__: prefix so the UI parses it: {out}"
        );
        assert!(out.contains(json), "marker payload must be embedded verbatim: {out}");
        assert!(out.contains("Image"), "follow-up text must reference Image kind: {out}");
        assert!(
            out.to_lowercase().contains("end your turn"),
            "must instruct LLM to stay quiet (image already in chat): {out}"
        );
    }

    #[test]
    fn inline_tool_result_voice_says_audio_not_image() {
        let json = r#"{"url":"/uploads/x.wav","mediaType":"audio/wav"}"#;
        let out = MediaKind::Voice.inline_tool_result(json);
        assert!(out.contains("Audio"), "voice must say Audio: {out}");
        assert!(!out.contains("Image"), "voice must NOT mention Image: {out}");
    }

    #[test]
    fn inline_tool_result_video_says_video() {
        let out = MediaKind::Video.inline_tool_result(r#"{"url":"/x.mp4","mediaType":"video/mp4"}"#);
        assert!(out.contains("Video"));
    }

    #[test]
    fn inline_tool_result_other_falls_back_to_media() {
        let out = MediaKind::Other.inline_tool_result(r#"{"url":"/x.bin","mediaType":"application/octet-stream"}"#);
        assert!(out.contains("Media"));
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
            // Fail DB ops fast (~200ms) instead of the default 30s acquire
            // timeout: the ui_session_does_not_panic_* tests intentionally hit
            // the notify()/upload DB path on this dead pool and must NOT hang
            // the CI suite waiting on 127.0.0.1:1.
            .acquire_timeout(std::time::Duration::from_millis(200))
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
            base_url:       "http://localhost:18789".into(),
            db:             fake_db(),
            upload_key:     [0u8; 32],
            retention_days: 30,
            provider_attempts: vec![],
            context:        context.clone(),
            agent_name:     "Arty".into(),
            tool_message_id: None,
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
    async fn provider_fails_over_to_next_in_chain_on_retryable_error() {
        // Exercises the run() chain-advance machinery that BOTH the retryable-
        // error arm and the Fix 1 timeout arm share (`i + 1 < attempts.len()`
        // → continue). The first provider returns a retryable 503; the loop
        // must advance to the second provider (200) and deliver exactly once.
        //
        // A real *timeout* failover can't be unit-tested here — the per-attempt
        // budget is 600s / attempts.len() (300s for this 2-chain), far too long
        // to wait on a genuinely hung mock — but the continue-to-next-provider
        // control flow is identical between the two arms, so this proves it.
        let server = MockServer::start().await;
        // First provider (X-Opex-Provider: dead) → 503 (retryable).
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .and(header("X-Opex-Provider", "dead"))
            .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
            .mount(&server)
            .await;
        // Second provider (X-Opex-Provider: live) → 200.
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .and(header("X-Opex-Provider", "live"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fakewav!"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let context = serde_json::json!({ "chat_id": 42, "channel": "telegram" });
        let mut task = make_task(&server.uri(), "send_voice", Some(router), context);
        task.provider_attempts = vec![
            vec![("X-Opex-Provider".into(), "dead".into())],
            vec![("X-Opex-Provider".into(), "live".into())],
        ];

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
        .expect("send_voice from the SECOND provider must arrive after failover");

        assert_eq!(action.name, "send_voice");
        assert!(
            action.params.get("audio_base64").is_some(),
            "failover success must still deliver the voice payload"
        );
        // Exactly one action delivered — no double-deliver across the chain.
        let _ = action.reply.send(Ok(()));
        run_handle.await.expect("task should complete without panic");
        assert!(
            rx.try_recv().is_err(),
            "chain must deliver exactly once, not once per attempt"
        );
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

    // ── deliver_to_ui notify wiring (#[sqlx::test], needs DB) ───────────────
    //
    // The web-bell `*_ready` notification was removed from deliver_to_ui —
    // these tests guard the regression where it silently comes back (e.g. a
    // future edit re-adding the notify() call on the success path). The
    // error-path notify (generation/save failure) is untouched and still
    // covered below.
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
            base_url:       "http://localhost:18789".into(),
            db,
            upload_key:     [0u8; 32],
            retention_days: 30,
            provider_attempts: vec![],
            context:        context.clone(),
            agent_name:     "Arty".into(),
            tool_message_id: None,
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

    async fn assert_no_ready_notification(pool: &sqlx::PgPool) {
        // deliver_to_ui must NOT insert a *_ready notification anymore.
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM notifications WHERE type LIKE '%\\_ready'",
        )
        .fetch_one(pool)
        .await
        .expect("count query must run");
        assert_eq!(count.0, 0, "no *_ready notification must be inserted");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_ui_voice_does_not_notify_ready(pool: sqlx::PgPool) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"OggS\x00\x00fakeaudio"))
            .mount(&server)
            .await;
        let task = make_task_with_db(&server.uri(), "send_voice", pool.clone(), serde_json::json!({}));
        task.run().await;
        assert_no_ready_notification(&pool).await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_ui_photo_save_failure_emits_image_error(pool: sqlx::PgPool) {
        // Force save_binary_to_uploads to fail by dropping the `uploads` table
        // on the test pool — INSERT will now error, but `notifications` stays
        // available so the error-path notify call still lands its row.
        sqlx::query("DROP TABLE uploads")
            .execute(&pool)
            .await
            .expect("drop uploads");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG"))
            .mount(&server)
            .await;
        let task = make_task_with_db(
            &server.uri(),
            "send_photo",
            pool.clone(),
            serde_json::json!({}),
        );
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

    // ── deliver_to_channel save+update mirror (QUICK-260508-0dj) ────────────
    //
    // These tests cover the post-channel-send mirror: when a Telegram-paired
    // session calls a YAML channel-action
    // tool, the bytes that successfully reach the channel must ALSO be
    // saved to uploads and the persisted tool message row prepended with a
    // `__file__:{json}\n` marker — best-effort, not allowed to regress the
    // channel-delivery promise. No web-bell notify is emitted on this path.

    /// Variant of `make_task_with_db` that lets the test pin a specific
    /// `tool_message_id` and a real `ChannelActionRouter` so we can drive the
    /// channel-send path to success / failure.
    fn make_task_with_db_router_msg_id(
        server_url: &str,
        action: &str,
        db: sqlx::PgPool,
        router: Option<ChannelActionRouter>,
        ctx_json: serde_json::Value,
        tool_message_id: Option<Uuid>,
    ) -> (BackgroundMediaTask, broadcast::Receiver<String>) {
        let (ui_tx, ui_rx) = broadcast::channel(8);
        let ca = ChannelActionConfig {
            action: action.to_string(),
            data_field: "_binary".into(),
        };
        let kind = MediaKind::from_action(&ca.action);
        let task = BackgroundMediaTask {
            tool:           make_tool(&format!("{server_url}/v1/audio/speech")),
            args:           serde_json::json!({ "input": "test", "_context": ctx_json }),
            ca,
            kind,
            http_client:    reqwest::Client::new(),
            resolver:       None,
            oauth_ctx:      None,
            channel_router: router,
            ui_event_tx:    Some(ui_tx),
            bg_tasks:       Arc::new(TaskTracker::new()),
            base_url:       "http://localhost:18789".into(),
            db,
            upload_key:     [0u8; 32],
            retention_days: 30,
            provider_attempts: vec![],
            context:        ctx_json.clone(),
            agent_name:     "Arty".into(),
            tool_message_id,
        };
        (task, ui_rx)
    }

    /// Insert a tool message row that the prepend can target. Returns the
    /// row id so the test can assert on its `content` afterwards.
    async fn insert_tool_row(pool: &sqlx::PgPool, original: &str) -> Uuid {
        let session_id = crate::db::sessions::create_new_session(
            pool,
            "Arty",
            "test-user",
            "telegram",
        )
        .await
        .expect("create_new_session");
        let row_id = Uuid::new_v4();
        crate::db::sessions::save_message_ex_with_id(
            pool,
            row_id,
            session_id,
            "tool",
            original,
            None,
            Some("call_xyz"),
            Some("Arty"),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("save_message_ex_with_id");
        row_id
    }

    async fn fetch_content(pool: &sqlx::PgPool, id: Uuid) -> String {
        sqlx::query_scalar("SELECT content FROM messages WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("messages row exists")
    }

    #[tokio::test]
    async fn deliver_to_channel_with_msg_id_none_still_sends_and_skips_prepend() {
        // Regression guard for the subagent / openai_compat / cron path:
        // when `tool_message_id` is None, the post-channel-send mirror still
        // attempts the uploads save, but the DB prepend is skipped because
        // there is no row to target (and no bell notify fires anymore).
        // Channel send must still happen unchanged.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let ctx_json = serde_json::json!({"chat_id": 42, "channel": "telegram"});
        let (task, _ui_rx) = make_task_with_db_router_msg_id(
            &server.uri(),
            "send_photo",
            fake_db(),
            Some(router),
            ctx_json,
            None, // ← off-the-record path
        );

        let run = tokio::spawn(task.run());
        let action = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                loop {
                    if let Ok(a) = rx.try_recv() {
                        return a;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await
        .expect("send_photo must arrive");
        assert_eq!(action.name, "send_photo");
        let _ = action.reply.send(Ok(()));
        run.await.expect("task completes");
        // No DB to assert on — the fake_db() pool would never connect anyway.
        // The point of this test is "no panic" + "channel still sent".
    }

    #[tokio::test]
    async fn deliver_to_channel_send_failure_skips_persist_and_notify() {
        // When the channel send fails (router replies Err), we MUST NOT
        // attempt save / DB prepend / notify — the user already saw the
        // channel error via the existing send_error_to_channel path, so
        // any extra notification would be double-noise.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake"))
            .mount(&server)
            .await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let ctx_json = serde_json::json!({"chat_id": 42, "channel": "telegram"});
        let (task, mut ui_rx) = make_task_with_db_router_msg_id(
            &server.uri(),
            "send_photo",
            fake_db(),
            Some(router),
            ctx_json,
            Some(Uuid::new_v4()), // would-be target row
        );

        let run = tokio::spawn(task.run());

        // 1) The send_photo action arrives — reply with Err to trigger the
        //    failure arm.
        let send_action = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                loop {
                    if let Ok(a) = rx.try_recv() {
                        return a;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await
        .expect("send_photo must arrive");
        assert_eq!(send_action.name, "send_photo");
        let _ = send_action.reply.send(Err("channel adapter error".into()));

        // 2) On the failure arm, the existing path emits a send_message
        //    error — drain it so the test isolates that no notify follows.
        let _err_action = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            async {
                loop {
                    if let Ok(a) = rx.try_recv() {
                        return a;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await
        .ok();

        run.await.expect("task completes");

        // 3) ui_event_tx received NO `<kind>_ready` notification.
        //    The broadcast channel is empty (or only has unrelated traffic).
        //    Use try_recv to check — Lagged/Closed/Empty all mean "no notify".
        match ui_rx.try_recv() {
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => {}
            Ok(payload) => panic!(
                "no UI notification expected on channel-send failure, got: {payload}"
            ),
            Err(broadcast::error::TryRecvError::Lagged(_)) => panic!(
                "no UI notification expected on channel-send failure (lagged)"
            ),
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    // reviewed: offsets from ASCII FILE_PREFIX const .len() and find('\n') — char boundaries
    #[allow(clippy::string_slice)]
    async fn deliver_to_channel_happy_path_prepends_file_marker(pool: sqlx::PgPool) {
        // Full happy path: channel send succeeds → save_binary_to_uploads
        // succeeds → prepend_message_content lands a `__file__:{...}\n`
        // marker on the persisted tool row. The original content is
        // preserved at the tail.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG\r\n\x1a\nfake"))
            .mount(&server)
            .await;

        let original = "[SYSTEM] Image dispatched in background; the user will receive a photo message. Do NOT mention image, picture, or generation.";
        let row_id = insert_tool_row(&pool, original).await;

        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let ctx_json = serde_json::json!({"chat_id": 42, "channel": "telegram"});
        let (task, _ui_rx) = make_task_with_db_router_msg_id(
            &server.uri(),
            "send_photo",
            pool.clone(),
            Some(router),
            ctx_json,
            Some(row_id),
        );

        let run = tokio::spawn(task.run());
        let action = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                loop {
                    if let Ok(a) = rx.try_recv() {
                        return a;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await
        .expect("send_photo must arrive");
        assert_eq!(action.name, "send_photo");
        let _ = action.reply.send(Ok(()));
        run.await.expect("task completes");

        let content = fetch_content(&pool, row_id).await;
        assert!(
            content.starts_with(crate::agent::engine::FILE_PREFIX),
            "prepended content must start with FILE_PREFIX; got: {content}"
        );
        assert!(
            content.ends_with(original),
            "original content must be preserved at tail; got: {content}"
        );
        // Marker JSON must contain a /uploads/ URL and a media-type.
        let prefix_len = content.find('\n').expect("marker line is newline-terminated");
        let marker = &content[crate::agent::engine::FILE_PREFIX.len()..prefix_len];
        let parsed: serde_json::Value =
            serde_json::from_str(marker).expect("marker must parse as JSON");
        assert!(
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("/api/uploads/")),
            "url must point at /api/uploads/{{id}}, got: {parsed}"
        );
        assert!(
            parsed.get("mediaType").is_some(),
            "mediaType must be present, got: {parsed}"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_channel_happy_path_does_not_notify_ready(pool: sqlx::PgPool) {
        // Same scenario as the prepend test — but the post-channel-send
        // mirror must NOT ping the web bell anymore (media already reached
        // the user via the channel); only the inline DB marker is mirrored.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake"))
            .mount(&server)
            .await;

        let row_id = insert_tool_row(&pool, "[SYSTEM] Image dispatched.").await;
        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let ctx_json = serde_json::json!({"chat_id": 42, "channel": "telegram"});
        let (task, mut ui_rx) = make_task_with_db_router_msg_id(
            &server.uri(),
            "send_photo",
            pool.clone(),
            Some(router),
            ctx_json,
            Some(row_id),
        );

        let run = tokio::spawn(task.run());
        let action = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                loop {
                    if let Ok(a) = rx.try_recv() {
                        return a;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await
        .expect("send_photo must arrive");
        let _ = action.reply.send(Ok(()));
        run.await.expect("task completes");

        // 1) No `*_ready` notification row is created anymore.
        assert_no_ready_notification(&pool).await;

        // 2) No UI broadcast fired either.
        match ui_rx.try_recv() {
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => {}
            Ok(payload) => panic!(
                "no UI notification expected on channel-send success, got: {payload}"
            ),
            Err(broadcast::error::TryRecvError::Lagged(_)) => panic!(
                "no UI notification expected on channel-send success (lagged)"
            ),
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn deliver_to_channel_save_failure_keeps_channel_send(pool: sqlx::PgPool) {
        // save_binary_to_uploads failure: channel delivery already happened
        // (the user has the bytes), so we MUST NOT abort. The DB prepend
        // never fires (no URL), notify never fires (the bell would be a lie),
        // but the channel send is preserved and the persisted row is
        // unchanged.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNGfake"))
            .mount(&server)
            .await;

        let original = "[SYSTEM] Image dispatched.";
        let row_id = insert_tool_row(&pool, original).await;
        let router = ChannelActionRouter::new();
        let (_conn_id, mut rx) = router.subscribe("telegram").await;
        let ctx_json = serde_json::json!({"chat_id": 42, "channel": "telegram"});
        let (task, mut ui_rx) = make_task_with_db_router_msg_id(
            &server.uri(),
            "send_photo",
            pool.clone(),
            Some(router),
            ctx_json,
            Some(row_id),
        );
        // Force save_binary_to_uploads to fail post-migration: dropping the
        // `uploads` table makes the INSERT error, but `messages` /
        // `notifications` remain available so the no-prepend / no-notify
        // assertions below still execute against a healthy schema.
        sqlx::query("DROP TABLE uploads")
            .execute(&pool)
            .await
            .expect("drop uploads");

        let run = tokio::spawn(task.run());
        let action = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            async {
                loop {
                    if let Ok(a) = rx.try_recv() {
                        return a;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            },
        )
        .await
        .expect("send_photo must arrive (channel send must NOT be aborted by upcoming save failure)");
        assert_eq!(action.name, "send_photo");
        let _ = action.reply.send(Ok(()));
        run.await.expect("task completes");

        // 1) Persisted row content is UNCHANGED — no `__file__:` marker
        //    because save failed before prepend could run.
        let content = fetch_content(&pool, row_id).await;
        assert_eq!(
            content, original,
            "save failure must leave the tool row content intact; got: {content}"
        );

        // 2) No UI notification fired (the bell would point at no URL).
        match ui_rx.try_recv() {
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => {}
            Ok(payload) => panic!(
                "save failure must not emit a notify, got: {payload}"
            ),
            Err(broadcast::error::TryRecvError::Lagged(_)) => panic!(
                "save failure must not emit a notify (lagged)"
            ),
        }

        // 3) `notifications` table has 0 rows for this run.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
            .fetch_one(&pool)
            .await
            .expect("notifications count");
        assert_eq!(count, 0, "no notification row must be inserted on save failure");
    }
}
