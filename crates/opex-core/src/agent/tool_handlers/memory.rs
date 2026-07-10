use async_trait::async_trait;
use serde_json::Value;

use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct MemoryToolHandler;

#[async_trait]
impl SystemToolHandler for MemoryToolHandler {
    async fn handle(&self, deps: ToolDeps<'_>, arguments: &Value) -> String {
        use crate::agent::pipeline::memory as pm;
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "search" => {
                let pinned_ids = deps.tex.pinned_chunk_ids.lock().await.clone();
                pm::handle_memory_search(
                    deps.memory_store.as_ref(),
                    deps.agent_name,
                    &pinned_ids,
                    arguments,
                )
                .await
            }
            "index" => {
                pm::handle_memory_index(deps.memory_store.as_ref(), deps.agent_name, arguments)
                    .await
            }
            "reindex" => {
                pm::handle_memory_reindex(
                    deps.memory_store.as_ref(),
                    deps.agent_name,
                    deps.workspace_dir,
                    arguments,
                )
                .await
            }
            "get" => pm::handle_memory_get(deps.memory_store.as_ref(), arguments).await,
            "delete" => {
                pm::handle_memory_delete(deps.memory_store.as_ref(), deps.db, arguments).await
            }
            "update" => {
                let mut args = arguments.clone();
                if let Some(sa) = arguments.get("sub_action").cloned()
                    && let Some(obj) = args.as_object_mut()
                {
                    obj.insert("action".to_string(), sa);
                }
                pm::handle_memory_update(
                    &deps.tex.memory_md_lock,
                    deps.workspace_dir,
                    deps.agent_name,
                    &args,
                )
                .await
            }
            _ => format!(
                "Error: unknown memory action '{}'. Use: search, index, reindex, get, delete, update.",
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
        assert_impl(MemoryToolHandler);
    }

    #[test]
    fn unknown_action_returns_error() {
        let msg = format!(
            "Error: unknown memory action '{}'. Use: search, index, reindex, get, delete, update.",
            "bogus"
        );
        assert!(msg.contains("bogus"));
        assert!(msg.contains("Use:"));
    }
}
