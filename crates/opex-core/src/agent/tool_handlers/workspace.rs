use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct WorkspaceWriteHandler;
pub struct WorkspaceReadHandler;
pub struct WorkspaceListHandler;
pub struct WorkspaceEditHandler;
pub struct WorkspaceDeleteHandler;
pub struct WorkspaceRenameHandler;

#[async_trait]
impl SystemToolHandler for WorkspaceWriteHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_write(
            deps.workspace_dir,
            deps.agent_name,
            deps.agent_base,
            deps.secrets.as_ref(),
            deps.signed_url_ttl_secs,
            args,
        )
        .await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceReadHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_read(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceListHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_list(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceEditHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_edit(
            deps.workspace_dir,
            deps.agent_name,
            deps.agent_base,
            deps.secrets.as_ref(),
            deps.signed_url_ttl_secs,
            args,
        )
        .await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceDeleteHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_delete(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceRenameHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_rename(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_workspace_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(WorkspaceWriteHandler);
        assert_impl(WorkspaceReadHandler);
        assert_impl(WorkspaceListHandler);
        assert_impl(WorkspaceEditHandler);
        assert_impl(WorkspaceDeleteHandler);
        assert_impl(WorkspaceRenameHandler);
    }
}
