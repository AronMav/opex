//! Code Assist API client sub-modules.
//!
//! Items declared here are used by Modules 3–4 once the provider wire-up
//! is complete. Allow dead_code / unused_imports at the module level so
//! clippy stays clean while the stubs are in place.
#![allow(dead_code, unused_imports)]

pub mod types;
pub mod schema;
pub mod request;
pub mod response;
pub mod project;
pub mod quota;

pub use types::{
    CODE_ASSIST_ENDPOINT, FREE_TIER_ID, LEGACY_TIER_ID,
    CodeAssistError, ProjectContext,
};
pub use schema::sanitize_gemini_tool_parameters;
pub use request::{build_gemini_request, wrap_code_assist_request};
pub use response::translate_gemini_response;
pub use project::ensure_project_ctx;
pub use quota::{QuotaBucket, retrieve_user_quota};

#[cfg(test)]
mod integration {
    use super::*;
    use opex_types::{Message, MessageRole, ToolDefinition};
    use serde_json::json;

    /// Smoke test: all six cross-module contract functions are callable with correct types.
    /// This test will fail to compile if any signature drifts from the spec contract.
    #[tokio::test]
    async fn cross_module_interface_contract_signatures_compile() {
        // 1. ProjectContext is constructible with the three required fields
        let _ctx = ProjectContext {
            project_id: "p".to_string(),
            managed_project_id: "m".to_string(),
            tier_id: "free-tier".to_string(),
        };

        // 2. ensure_project_ctx signature: (access_token: &str, stored: Option<&str>) -> Result<ProjectContext, CodeAssistError>
        // We call it but expect ProjectIdRequired since there's no HTTP server.
        // This is a compile-time contract check, not a runtime assertion.
        let _result: Result<ProjectContext, CodeAssistError> =
            ensure_project_ctx("fake-token", None).await;

        // 3. build_gemini_request signature
        let msgs = vec![Message {
            role: MessageRole::User,
            content: "hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let tools: Vec<ToolDefinition> = vec![];
        let _req: serde_json::Value = build_gemini_request(&msgs, &tools, None, json!({}));

        // 4. wrap_code_assist_request signature
        let _wrapped: serde_json::Value =
            wrap_code_assist_request("project", "gemini-2.5-pro", "uuid-here", json!({}));

        // 5. translate_gemini_response signature
        let _resp = translate_gemini_response(json!({ "response": { "candidates": [] } }));

        // 6. sanitize_gemini_tool_parameters signature
        let _schema = sanitize_gemini_tool_parameters(json!({ "type": "object" }));
    }
}
