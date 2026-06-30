use opex_types::IncomingMessage;

/// Well-known channel identifiers used throughout the engine.
pub mod channel {
    pub const CRON: &str = "cron";
    pub const HEARTBEAT: &str = "heartbeat";
    pub const SYSTEM: &str = "system";
    pub const INTER_AGENT: &str = "inter-agent";
    pub const UI: &str = "ui";
    // GROUP / TELEGRAM are part of the named-channel registry; the runtime
    // matches on string literals from the channels/ adapter, not these
    // constants, so they appear unused but are kept as the canonical names.
    #[allow(dead_code)]
    pub const GROUP: &str = "group";
    #[allow(dead_code)]
    pub const TELEGRAM: &str = "telegram";

    /// Returns true for automated channels that bypass approval checks.
    pub fn is_automated(ch: &str) -> bool {
        matches!(ch, CRON | HEARTBEAT | SYSTEM | INTER_AGENT)
    }
}

/// Chat id for per-chat voice mode (`/voice` command + auto-tts).
///
/// Messaging-channel turns (Telegram, …) carry the id in `context["chat_id"]`.
/// Web/UI turns send an empty context, so the agent name is used as a stable
/// per-agent UI "chat" — this makes `/voice on` and auto-tts work on the web UI
/// too, not only on messaging channels. Returns `None` when no usable id exists
/// (e.g. a non-UI channel turn without a `chat_id`).
pub fn voice_chat_id(msg: &IncomingMessage) -> Option<String> {
    if let Some(v) = msg.context.get("chat_id") {
        let s = v.to_string().trim_matches('"').to_string();
        if !s.is_empty() && s != "null" {
            return Some(s);
        }
    }
    if msg.channel == channel::UI && !msg.agent_id.is_empty() {
        return Some(msg.agent_id.clone());
    }
    None
}

#[cfg(test)]
mod voice_chat_id_tests {
    use super::*;

    fn msg(ch: &str, agent: &str, ctx: serde_json::Value) -> IncomingMessage {
        IncomingMessage {
            user_id: "u".into(),
            context: ctx,
            text: Some("hi".into()),
            attachments: vec![],
            agent_id: agent.into(),
            channel: ch.into(),
            timestamp: chrono::Utc::now(),
            formatting_prompt: None,
            tool_policy_override: None,
            leaf_message_id: None,
            user_message_id: None,
        }
    }

    #[test]
    fn channel_turn_uses_context_chat_id() {
        let m = msg("telegram", "Arty", serde_json::json!({"chat_id": "42"}));
        assert_eq!(voice_chat_id(&m).as_deref(), Some("42"));
    }

    #[test]
    fn ui_turn_falls_back_to_agent_name() {
        // Web UI sends an empty context — voice mode keys on the agent name.
        let m = msg(channel::UI, "Arty", serde_json::json!({}));
        assert_eq!(voice_chat_id(&m).as_deref(), Some("Arty"));
    }

    #[test]
    fn non_ui_without_chat_id_is_none() {
        let m = msg("telegram", "Arty", serde_json::json!({}));
        assert_eq!(voice_chat_id(&m), None);
    }

    #[test]
    fn null_or_empty_chat_id_falls_through() {
        // null/empty context chat_id on UI → agent name; on a channel → None.
        let ui = msg(channel::UI, "Arty", serde_json::json!({"chat_id": null}));
        assert_eq!(voice_chat_id(&ui).as_deref(), Some("Arty"));
        let tg = msg("telegram", "Arty", serde_json::json!({"chat_id": ""}));
        assert_eq!(voice_chat_id(&tg), None);
    }
}

/// Tool execution categories for approval system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    System,
    Destructive,
    External,
}

impl ToolCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Destructive => "destructive",
            Self::External => "external",
        }
    }

    pub fn classify(tool_name: &str) -> Self {
        match tool_name {
            "code_exec" | "process" | "browser_action" => Self::System,
            "workspace_delete" | "workspace_write" | "workspace_edit" | "workspace_rename" => Self::Destructive,
            n if n.starts_with("git_") => Self::System,
            _ => Self::External,
        }
    }
}
