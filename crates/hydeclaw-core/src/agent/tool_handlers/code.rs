use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::sandbox as ps;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct CodeExecHandler;

#[async_trait]
impl SystemToolHandler for CodeExecHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ps::handle_code_exec(
            args,
            deps.agent_name,
            deps.agent_base,
            deps.sandbox,
            deps.workspace_dir,
            deps.secrets.as_ref(),
            deps.signed_url_ttl_secs,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(CodeExecHandler);
    }
}
