use async_trait::async_trait;
use serde_json::Value;

use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct AgentToolHandler;
pub struct AgentsListHandler;

#[async_trait]
impl SystemToolHandler for AgentToolHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        crate::agent::pipeline::agent_tool::handle_agent_tool(
            deps.session_pools,
            deps.agent_map,
            deps.db,
            deps.agent_name,
            args,
            deps.agent_tool_timeouts,
        )
        .await
    }
}

#[async_trait]
impl SystemToolHandler for AgentsListHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        crate::agent::pipeline::sessions::handle_agents_list(
            deps.agent_map,
            deps.session_pools,
            deps.agent_name,
            args,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(AgentToolHandler);
        assert_impl(AgentsListHandler);
    }
}
