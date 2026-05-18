//! Pipeline step: channel actions — handle_message_action, send_channel_message,
//! execute_yaml_channel_action, and helper constructors.
//! Extracted from engine_handlers.rs as free functions taking &CommandContext.

use std::sync::Arc;

use super::CommandContext;
use crate::agent::channel_actions::ChannelAction;
use crate::config::AppConfig;
use crate::secrets::SecretsManager;
use crate::tools::yaml_tools::OAuthContext;
use crate::oauth::OAuthManager;

/// Build the public base URL for signed `/api/uploads/{id}` links.
/// Mirrors the pattern in `gateway/handlers/agents/icon.rs::public_base` and
/// `gateway/handlers/media.rs:87-92`. Falls back to `http://localhost:{port}`
/// when `[gateway] public_url` is not set, so background tasks (which have no
/// State extractor) can still mint URLs that point at the same host.
pub(crate) fn public_base_for_uploads(app_config: &AppConfig) -> String {
    if let Some(ref pu) = app_config.gateway.public_url {
        pu.trim_end_matches('/').to_string()
    } else {
        let port = app_config
            .gateway
            .listen
            .rsplit(':')
            .next()
            .unwrap_or("18789");
        format!("http://localhost:{port}")
    }
}

/// Build a `SecretsEnvResolver` for YAML tool env resolution.
pub(crate) fn make_resolver(
    secrets: &Arc<SecretsManager>,
    agent_name: &str,
) -> crate::agent::engine::SecretsEnvResolver {
    crate::agent::engine::SecretsEnvResolver {
        secrets: secrets.clone(),
        agent_name: agent_name.to_string(),
    }
}

/// Build `OAuthContext` for provider-based YAML tool auth.
pub(crate) fn make_oauth_context(
    oauth: Option<&Arc<OAuthManager>>,
    agent_name: &str,
) -> Option<OAuthContext> {
    oauth.map(|mgr| OAuthContext {
        manager: mgr.clone(),
        agent_id: agent_name.to_string(),
    })
}

/// Internal tool: perform message actions via channel router.
pub async fn handle_message_action(ctx: &CommandContext<'_>, args: &serde_json::Value) -> String {
    let router = match &ctx.state.channel_router {
        Some(r) => r,
        None => return "Error: message actions not available (no channel connection)".to_string(),
    };

    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if action.is_empty() {
        return "Error: 'action' is required".to_string();
    }

    let context = args.get("_context").cloned().unwrap_or(serde_json::Value::Null);
    let target_channel = args.get("channel").and_then(|v| v.as_str()).map(|s| s.to_string());

    // Collect action-specific params (exclude internal _context, action, channel fields)
    let params = {
        let mut p = serde_json::Map::new();
        if let Some(obj) = args.as_object() {
            for (k, v) in obj {
                if k != "_context" && k != "action" && k != "channel" {
                    p.insert(k.clone(), v.clone());
                }
            }
        }
        serde_json::Value::Object(p)
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

    let channel_action = ChannelAction {
        name: action.to_string(),
        params,
        context,
        reply: reply_tx,
        target_channel,
    };

    if let Err(e) = router.send(channel_action).await {
        return format!("Error: {e}");
    }

    match tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx).await {
        Ok(Ok(Ok(()))) => format!("Successfully performed '{}' action", action),
        Ok(Ok(Err(e))) => format!("Error performing '{}': {}", action, e),
        Ok(Err(_)) => "Error: action reply channel dropped".to_string(),
        Err(_) => "Error: action timed out".to_string(),
    }
}

/// Send a message to a specific channel directly (e.g. from cron announce).
/// Uses channel router to route to the correct channel adapter.
pub async fn send_channel_message(
    ctx: &CommandContext<'_>,
    channel: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let router = ctx.state.channel_router.as_ref()
        .ok_or_else(|| anyhow::anyhow!("no channel connection available"))?;
    let (reply_tx, _) = tokio::sync::oneshot::channel();
    let action = ChannelAction {
        name: "send_message".to_string(),
        params: serde_json::json!({ "text": text }),
        context: serde_json::json!({ "channel": channel, "chat_id": chat_id }),
        reply: reply_tx,
        target_channel: Some(channel.to_string()),
    };
    router.send(action).await.map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

/// Execute a system YAML tool that has a channel_action (e.g. TTS -> send_voice,
/// generate_image -> send_photo, future video generators -> send_video).
///
/// Two delivery paths depending on whether the call originates from a chat
/// channel (Telegram/Discord/etc.) or a web-UI session:
///
/// - **chat channel** (`chat_id` present): defer to a `BackgroundMediaTask`
///   so a slow generator (Qwen3-TTS / FLUX on Pi) cannot block or time out
///   the active SSE session. The media is delivered out-of-band via the
///   channel adapter (`send_photo` / `send_voice` / ...).
/// - **web UI** (no `chat_id`): generate inline so the media renders in the
///   chat stream itself (via `__file__:` marker that chat-history.ts parses
///   into an inline image / audio / video element). The user sees it in
///   place rather than only in the notification bell.
pub async fn execute_yaml_channel_action(
    ctx: &CommandContext<'_>,
    tool: &crate::tools::yaml_tools::YamlToolDef,
    args: &serde_json::Value,
    ca: &crate::tools::yaml_tools::ChannelActionConfig,
) -> String {
    let context = args.get("_context").cloned().unwrap_or(serde_json::Value::Null);
    let has_channel = context.get("chat_id").is_some();

    if !has_channel {
        return execute_inline_for_ui(ctx, tool, args, ca).await;
    }

    let task =
        crate::agent::pipeline::media_background::BackgroundMediaTask::from_ctx(ctx, tool, args, ca);
    task.spawn()
}

/// Synchronous web-UI delivery: generate the media bytes, save them to
/// `workspace/uploads/`, and return a tool result whose first line is a
/// `__file__:` marker. The UI's chat-history reducer turns that into an
/// inline preview in place. No notification bell row is created — duplicating
/// what's already inline would be noise.
async fn execute_inline_for_ui(
    ctx: &CommandContext<'_>,
    tool: &crate::tools::yaml_tools::YamlToolDef,
    args: &serde_json::Value,
    ca: &crate::tools::yaml_tools::ChannelActionConfig,
) -> String {
    use crate::agent::pipeline::handlers::save_binary_to_uploads;
    use crate::agent::pipeline::media_background::{provider_header_for, MediaKind};

    let kind = MediaKind::from_action(&ca.action);
    let resolver = make_resolver(&ctx.tex.secrets, &ctx.cfg.agent.name);
    let oauth_ctx = make_oauth_context(ctx.tex.oauth.as_ref(), &ctx.cfg.agent.name);

    let mut tool_headers: Vec<(String, String)> = Vec::new();
    if let Some(header) = provider_header_for(
        kind,
        ctx.cfg.agent.tts_provider.as_deref(),
        ctx.cfg.agent.imagegen_provider.as_deref(),
    ) {
        tool_headers.push(header);
    }

    // Lift the per-tool timeout the same way BackgroundMediaTask does — UI
    // sessions still have to wait, but FLUX / Qwen3-TTS on Pi can take
    // 30-120s and the YAML default of 60s is too tight.
    let mut bg_tool = tool.clone();
    if bg_tool.timeout < 600 {
        bg_tool.timeout = 600;
    }

    // Fresh long-timeout client so reqwest doesn't abort at the shared
    // engine 120s deadline. Mirrors BackgroundMediaTask::from_ctx.
    let bg_http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .unwrap_or_else(|_| ctx.tex.http_client.clone());

    let bytes = match bg_tool
        .execute_binary(
            args,
            &bg_http_client,
            Some(&resolver as &dyn crate::tools::yaml_tools::EnvResolver),
            oauth_ctx.as_ref(),
            &tool_headers,
        )
        .await
    {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(tool = %tool.name, kind = ?kind, error = %e, "inline media generation failed");
            return format!("Error: media generation failed: {e}");
        }
    };

    let upload_key = ctx.tex.secrets.get_upload_hmac_key();
    let base_url = public_base_for_uploads(&ctx.cfg.app_config);
    let (url, media_type) = match save_binary_to_uploads(
        &ctx.cfg.db,
        ctx.cfg.app_config.cleanup.uploads_retention_days,
        &bytes,
        kind.upload_hint(),
        &upload_key,
        &base_url,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(tool = %tool.name, kind = ?kind, error = %e, "inline media save failed");
            return format!("Error: save_to_uploads failed: {e}");
        }
    };

    tracing::info!(
        tool = %tool.name, kind = ?kind, url = %url, mime = %media_type,
        "inline media delivered to web UI"
    );

    let marker_json = serde_json::json!({"url": url, "mediaType": media_type}).to_string();
    kind.inline_tool_result(&marker_json)
}
