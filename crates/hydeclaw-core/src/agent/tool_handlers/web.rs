use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::subagent as psub;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct WebFetchHandler;

#[async_trait]
impl SystemToolHandler for WebFetchHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        if let Some(u) = args.get("url").and_then(|v| v.as_str())
            && crate::tools::url_policy::url_blocked(u, &deps.cfg.app_config.security.blocked_domains)
        {
            return format!("⛔ blocked by domain policy: {u}");
        }
        psub::handle_web_fetch(
            deps.http_client,
            &deps.toolgate_url,
            deps.gateway_listen,
            args,
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
        assert_impl(WebFetchHandler);
    }
}
