//! Pipeline step: channel actions — handle_message_action, send_channel_message,
//! execute_yaml_channel_action, and helper constructors.
//! Extracted from engine_handlers.rs as free functions taking &CommandContext.

use std::sync::Arc;

use super::CommandContext;
use crate::agent::channel_actions::ChannelAction;
use crate::secrets::SecretsManager;
use crate::tools::yaml_tools::OAuthContext;
use crate::oauth::OAuthManager;

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
/// Dispatches a `BackgroundMediaTask` so slow synthesis / generation (Qwen3-TTS,
/// FLUX, etc. on Pi) cannot block or time out the active SSE session. Returns
/// the LLM-facing system instruction; the actual media is delivered out-of-band.
pub async fn execute_yaml_channel_action(
    ctx: &CommandContext<'_>,
    tool: &crate::tools::yaml_tools::YamlToolDef,
    args: &serde_json::Value,
    ca: &crate::tools::yaml_tools::ChannelActionConfig,
) -> String {
    let task =
        crate::agent::pipeline::media_background::BackgroundMediaTask::from_ctx(ctx, tool, args, ca);
    task.spawn()
}
