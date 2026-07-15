use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct ToolCreateHandler;
pub struct ToolListHandler;
pub struct ToolTestHandler;
pub struct ToolVerifyHandler;
pub struct ToolDisableHandler;
pub struct ToolDiscoverHandler;

#[async_trait]
impl SystemToolHandler for ToolCreateHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_create(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolListHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_list(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolTestHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_test(
            deps.workspace_dir,
            &deps.cfg.profile_slots,
            deps.http_client,
            deps.ssrf_client,
            deps.secrets,
            deps.agent_name,
            deps.oauth.as_ref(),
            args,
        )
        .await
    }
}

#[async_trait]
impl SystemToolHandler for ToolVerifyHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_verify(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolDisableHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_disable(deps.workspace_dir, args).await
    }
}

#[async_trait]
impl SystemToolHandler for ToolDiscoverHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_tool_discover(deps.workspace_dir, deps.ssrf_client, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(ToolCreateHandler);
        assert_impl(ToolListHandler);
        assert_impl(ToolTestHandler);
        assert_impl(ToolVerifyHandler);
        assert_impl(ToolDisableHandler);
        assert_impl(ToolDiscoverHandler);
    }
}
