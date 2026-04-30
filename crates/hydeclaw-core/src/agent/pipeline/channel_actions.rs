//! Pipeline step: channel actions — handle_message_action, send_channel_message,
//! execute_yaml_channel_action, and helper constructors.
//! Extracted from engine_handlers.rs as free functions taking &CommandContext.

use std::sync::Arc;

use super::CommandContext;
use crate::agent::channel_actions::ChannelAction;
use crate::agent::pipeline::handlers as ph;
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

/// Execute a system YAML tool that has a channel_action (e.g. TTS -> send_voice, screenshot -> send_photo).
/// Calls the tool HTTP endpoint for binary data, then sends it via channel router.
/// For media actions (send_photo, send_voice), also saves to uploads/ and returns a FILE_PREFIX marker
/// so the UI can display the media inline (image preview / audio player).
pub async fn execute_yaml_channel_action(
    ctx: &CommandContext<'_>,
    tool: &crate::tools::yaml_tools::YamlToolDef,
    args: &serde_json::Value,
    ca: &crate::tools::yaml_tools::ChannelActionConfig,
) -> String {
    let resolver = make_resolver(&ctx.tex.secrets, &ctx.cfg.agent.name);
    let oauth_ctx = make_oauth_context(ctx.tex.oauth.as_ref(), &ctx.cfg.agent.name);
    tracing::info!(tool = %tool.name, action = %ca.action, "executing channel action: calling tool endpoint");
    // Internal endpoints (toolgate, searxng, etc.) bypass SSRF filtering
    let client = if crate::tools::ssrf::is_internal_endpoint(&tool.endpoint) {
        &ctx.tex.http_client
    } else {
        &ctx.tex.ssrf_http_client
    };
    let data_bytes = match tool.execute_binary(args, client, Some(&resolver), oauth_ctx.as_ref()).await {
        Ok(b) => b,
        Err(e) => return format!("Error calling tool '{}': {}", tool.name, e),
    };
    tracing::info!(tool = %tool.name, bytes = data_bytes.len(), "channel action: got binary data");

    // --- Save image/media to uploads/ for UI display ---
    // Phase 64 SEC-03: signed URL — key via HKDF from master, TTL from config.
    let media_hint = match ca.action.as_str() {
        "send_photo" => Some("image"),
        "send_voice" => Some("audio"),
        _ => None,
    };
    let file_marker = if let Some(hint) = media_hint {
        let upload_key = ctx.tex.secrets.get_upload_hmac_key();
        let ttl_secs = ctx.cfg.app_config.uploads.signed_url_ttl_secs;
        match ph::save_binary_to_uploads(
            &ctx.cfg.workspace_dir,
            &data_bytes,
            hint,
            &upload_key,
            ttl_secs,
        ).await {
            Ok((url, media_type)) => {
                let meta = serde_json::json!({"url": url, "mediaType": media_type});
                Some(format!("{}{}", crate::agent::engine::FILE_PREFIX, meta))
            }
            Err(e) => {
                tracing::warn!(error = %e, hint = %hint, "failed to save media to uploads for UI");
                None
            }
        }
    } else {
        None
    };

    // --- Send via channel router (Telegram etc.) ---
    // Skip channel action for UI sessions (no chat_id in context).
    let context = args.get("_context").cloned().unwrap_or(serde_json::Value::Null);
    let has_channel_context = context.get("chat_id").is_some();

    let channel_result = if !has_channel_context {
        tracing::info!(tool = %tool.name, "skipping channel action: no chat_id in context (UI session)");
        None
    } else if let Some(ref router) = ctx.state.channel_router {
        use base64::Engine as _;
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&data_bytes);
        tracing::info!(tool = %tool.name, context = %context, "channel action: sending to adapter");

        let param_key = match ca.action.as_str() {
            "send_photo" => "image_base64",
            "send_voice" => "audio_base64",
            other => {
                tracing::warn!(action = %other, "unknown channel action, using 'data_base64'");
                "data_base64"
            }
        };

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if router
            .send(ChannelAction {
                name: ca.action.clone(),
                params: serde_json::json!({ param_key: data_base64 }),
                context,
                reply: reply_tx,
                target_channel: None,
            })
            .await
            .is_err()
        {
            Some("Error: channel action channel closed".to_string())
        } else {
            match tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx).await {
                Ok(Ok(Ok(()))) => Some(format!("{} sent successfully", ca.action)),
                Ok(Ok(Err(e))) => Some(format!("Error sending {}: {}", ca.action, e)),
                Ok(Err(_)) => Some(format!("Error: {} reply channel dropped", ca.action)),
                Err(_) => Some(format!("Error: {} send timed out", ca.action)),
            }
        }
    } else {
        None
    };

    // Return file marker (for UI) + channel result
    match (file_marker, channel_result) {
        (Some(marker), Some(ch_res)) => format!("{}\n{}", marker, ch_res),
        (Some(marker), None) => format!(
            "{}\nUI session: media rendered inline as a player/preview. The user already sees/hears it. Do NOT claim it was sent to Telegram or any other channel — there is no chat_id here. Do NOT call this tool again for the same content.",
            marker
        ),
        (None, Some(ch_res)) => ch_res,
        (None, None) => "Error: no channel connection and failed to save media".to_string(),
    }
}
