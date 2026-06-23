use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct SecretSetHandler;

#[async_trait]
impl SystemToolHandler for SecretSetHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_secret_set(deps.secrets, deps.agent_name, deps.agent_base, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(SecretSetHandler);
    }
}
