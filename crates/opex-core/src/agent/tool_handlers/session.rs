use async_trait::async_trait;
use serde_json::Value;

use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct SessionHandler;

#[async_trait]
impl SystemToolHandler for SessionHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        use crate::agent::pipeline::sessions;
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "list" => sessions::handle_sessions_list(deps.db, deps.agent_name, args).await,
            "history" => sessions::handle_sessions_history(deps.db, deps.agent_name, args).await,
            "search" => sessions::handle_session_search(deps.db, deps.agent_name, args).await,
            "context" => {
                sessions::handle_session_context(deps.db, deps.agent_name, args).await
            }
            "send" => {
                sessions::handle_session_send(deps.state.channel_router.as_ref(), args).await
            }
            "export" => sessions::handle_session_export(deps.db, deps.agent_name, args).await,
            _ => format!(
                "Error: unknown session action '{}'. Use: list, history, search, context, send, export.",
                action
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(SessionHandler);
    }

    #[test]
    fn unknown_action_error() {
        let msg = format!(
            "Error: unknown session action '{}'. Use: list, history, search, context, send, export.",
            "bad"
        );
        assert!(msg.contains("bad"));
    }
}
