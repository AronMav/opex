//! Stage C phase 2A: resolve owner's channel + chat_id for delivering initiative
//! proposals and goal results. SECURITY (H1): the caller MUST pass owner_id
//! sourced from agent config (engine.cfg().agent.access.owner_id), never a request.
use sqlx::PgPool;

use crate::agent::channel_actions::{ChannelAction, ChannelActionRouter};

pub(crate) fn parse_chat_id(owner_id: Option<&str>) -> Option<i64> {
    owner_id?.trim().parse::<i64>().ok()
}

pub async fn resolve_owner_target(db: &PgPool, agent_name: &str, owner_id: Option<&str>) -> Option<(String, i64)> {
    let chat_id = parse_chat_id(owner_id)?;
    let ch: Option<String> = sqlx::query_scalar(
        "SELECT channel_type FROM agent_channels
         WHERE agent_name = $1 AND channel_type = 'telegram' AND status = 'running'
         ORDER BY created_at LIMIT 1",
    ).bind(agent_name).fetch_optional(db).await.ok().flatten();
    ch.map(|c| (c, chat_id))
}

/// Deliver an initiative proposal to the owner's channel (e.g. Telegram).
/// Fire-and-forget with a bounded wait: throwaway oneshot reply + 5s timeout,
/// result ignored — matches the fail-soft posture of the rest of the tick.
pub async fn send_proposal_to_channel(
    router: &ChannelActionRouter, channel: &str, chat_id: i64,
    proposal_id: uuid::Uuid, text: &str, rationale: &str,
) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let action = ChannelAction {
        name: "initiative_proposal".to_string(),
        params: serde_json::json!({ "proposal_id": proposal_id.to_string(), "text": text, "rationale": rationale }),
        context: serde_json::json!({ "chat_id": chat_id }),
        reply: reply_tx,
        target_channel: Some(channel.to_string()),
    };
    if router.send(action).await.is_ok() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await;
    }
}

/// Pure: numbered list of all N intents for the owner's approval message.
pub(crate) fn day_plan_body(intents: &[String]) -> String {
    intents.iter().enumerate().map(|(i, t)| format!("{}. {t}", i + 1)).collect::<Vec<_>>().join("\n")
}

/// Pure: informational auto-approve message — header + numbered intents.
pub(crate) fn day_plan_auto_approved_body(agent: &str, intents: &[String]) -> String {
    format!("🤖 {agent}: план на день принят автоматически\n{}", day_plan_body(intents))
}

/// Pure: pause notice when the daily token budget is reached.
pub(crate) fn day_plan_paused_text(agent: &str, cap: u64) -> String {
    format!("⏸ {agent}: дневной лимит {cap} токенов достигнут — план приостановлен до завтра")
}

/// Deliver the morning day-plan (ALL intents enumerated) to the owner's channel.
/// `date` (plan generation date) is embedded in the button callback (review H2).
pub async fn send_day_plan_to_channel(router: &ChannelActionRouter, channel: &str, chat_id: i64, intents: &[String], date: chrono::NaiveDate) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let action = ChannelAction {
        name: "day_plan".to_string(),
        params: serde_json::json!({ "intents": intents, "date": date.to_string() }),
        context: serde_json::json!({ "chat_id": chat_id }),
        reply: reply_tx,
        target_channel: Some(channel.to_string()),
    };
    if router.send(action).await.is_ok() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn chat_id_parses_only_numeric_owner() {
        assert_eq!(super::parse_chat_id(Some("388443751")), Some(388443751));
        assert_eq!(super::parse_chat_id(Some("not-a-number")), None);
        assert_eq!(super::parse_chat_id(None), None);
        assert_eq!(super::parse_chat_id(Some("")), None);
    }

    #[test]
    fn day_plan_body_numbers_all_intents() {
        let body = super::day_plan_body(&["довести X".to_string(), "разобрать Y".to_string()]);
        assert!(body.contains("1.") && body.contains("довести X"));
        assert!(body.contains("2.") && body.contains("разобрать Y"));
    }

    #[test]
    fn paused_text_names_agent_and_cap() {
        let s = super::day_plan_paused_text("Arty", 200_000);
        assert!(s.contains("Arty"));
        assert!(s.contains("200000"));
    }

    #[test]
    fn auto_approved_body_has_header_and_all_intents() {
        let s = super::day_plan_auto_approved_body("Arty", &["довести X".to_string(), "разобрать Y".to_string()]);
        assert!(s.contains("Arty"));
        assert!(s.contains("автоматически"));
        assert!(s.contains("довести X") && s.contains("разобрать Y"));
    }
}
