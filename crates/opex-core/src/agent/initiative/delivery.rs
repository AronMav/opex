//! Stage C phase 2A: resolve owner's channel + chat_id for delivering initiative
//! proposals and goal results. SECURITY (H1): the caller MUST pass owner_id
//! sourced from agent config (engine.cfg().agent.access.owner_id), never a request.
use sqlx::PgPool;

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

#[cfg(test)]
mod tests {
    #[test]
    fn chat_id_parses_only_numeric_owner() {
        assert_eq!(super::parse_chat_id(Some("388443751")), Some(388443751));
        assert_eq!(super::parse_chat_id(Some("not-a-number")), None);
        assert_eq!(super::parse_chat_id(None), None);
        assert_eq!(super::parse_chat_id(Some("")), None);
    }
}
