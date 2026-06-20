use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct TodoHandler;

#[async_trait]
impl SystemToolHandler for TodoHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_todo(deps.db, deps.session_id, args).await
    }
}
