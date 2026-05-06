//! Tool dispatch: execute_tool_call, execute_tool_call_inner, approval flow,
//! usage recording, and tool policy filtering.
//! Extracted from engine.rs for readability.

use super::*;
use crate::agent::context_builder::ContextBuilderDeps;

impl AgentEngine {
    /// Execute a tool call — routes to internal tools, MCP servers, or ToolRegistry.
    /// Returns a boxed future to allow recursive calls (approval re-injection → execute_tool_call).
    pub(super) fn execute_tool_call<'a>(
        &'a self,
        name: &'a str,
        arguments: &'a serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            let audit_start = std::time::Instant::now();
            let result = self.execute_tool_call_inner(name, arguments).await;

            // Audit record (dispatched via bounded queue)
            let elapsed = audit_start.elapsed();
            let duration_ms = elapsed.as_millis() as i32;
            let is_error = crate::agent::pipeline::dispatch::classify_tool_result(&result);
            let (status, error_msg) = if is_error {
                ("error", Some(result.clone()))
            } else {
                ("ok", None)
            };

            // Phase 65 OBS-02: record tool latency histogram. `status` is
            // bounded-cardinality ("ok" / "error") — safe label.
            self.cfg().metrics.record_tool_latency(
                name,
                &self.cfg().agent.name,
                status,
                elapsed,
            );

            // Extract session_id from enriched _context
            let session_id = arguments
                .get("_context")
                .and_then(|c| c.get("session_id"))
                .and_then(|s| s.as_str())
                .and_then(|s| Uuid::parse_str(s).ok());

            // Strip _context from parameters before storing (contains internal routing data)
            let clean_params = crate::agent::pipeline::dispatch::clean_tool_params(arguments);

            // Hook: AfterToolResult (fire-and-forget, non-blocking)
            let hook_event = crate::agent::hooks::HookEvent::AfterToolResult {
                agent: self.cfg().agent.name.clone(),
                tool_name: name.to_string(),
                duration_ms: duration_ms as u64,
            };
            self.hooks().fire(&hook_event);
            self.hooks().fire_webhooks(&hook_event);

            self.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::ToolExecution {
                agent_name: self.cfg().agent.name.clone(),
                session_id,
                tool_name: name.to_string(),
                parameters: Some(clean_params),
                status: status.to_string(),
                duration_ms: Some(duration_ms),
                error: error_msg.clone(),
            });

            // Record tool quality (non-system tools only)
            if !super::all_system_tool_names().contains(&name) {
                self.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::ToolQuality {
                    tool_name: name.to_string(),
                    success: !is_error,
                    duration_ms,
                    error: error_msg,
                });
            }

            result
        })
    }

    /// Inner tool dispatch (separated for audit wrapping).
    pub(super) fn execute_tool_call_inner<'a>(
        &'a self,
        name: &'a str,
        arguments: &'a serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            // 0. Approval check — if tool requires confirmation, wait for owner.
            // Skip approval for automated channels (cron, heartbeat, inter-agent).
            let context = arguments.get("_context").cloned().unwrap_or_default();
            let is_automated = context.get("_channel")
                .and_then(|v| v.as_str())
                .map(crate::agent::channel_kind::channel::is_automated)
                .unwrap_or(false);
            let has_interactive_channel = context.get("chat_id").is_some() && !is_automated;
            if self.needs_approval(name) && has_interactive_channel {
                // Skip if tool is in allowlist
                if let Ok(true) = crate::db::approvals::check_allowlist(&self.cfg().db, &self.cfg().agent.name, name).await {
                    // fall through to execution
                } else {
                    let timeout_secs = self.cfg().agent.approval
                        .as_ref()
                        .map(|a| a.timeout_seconds)
                        .unwrap_or(300);

                    let outcome = self.cfg().approval_manager.request_approval(
                        &self.cfg().agent.name,
                        name,
                        arguments,
                        &context,
                        timeout_secs,
                        self.state().channel_router.as_ref(),
                        self.state().ui_event_tx.as_ref(),
                        self.sse_event_tx(),
                    ).await;

                    use crate::agent::approval_manager::ApprovalOutcome;
                    match outcome {
                        ApprovalOutcome::Approved => { /* fall through to execution */ }
                        ApprovalOutcome::ApprovedWithModifiedArgs(mut modified) => {
                            // Preserve internal _context from original arguments
                            if let Some(ctx) = arguments.get("_context")
                                && let Some(obj) = modified.as_object_mut()
                            {
                                obj.insert("_context".to_string(), ctx.clone());
                            }
                            return self.execute_tool_call(name, &modified).await;
                        }
                        ApprovalOutcome::Rejected(reason) => return reason,
                        ApprovalOutcome::Cancelled => {
                            return format!("Tool `{}` approval was cancelled.", name);
                        }
                        ApprovalOutcome::TimedOut { timeout_secs } => {
                            return format!("Tool `{}` approval timed out after {}s.", name, timeout_secs);
                        }
                    }
                }
            }

            // Hook: BeforeToolCall
            let hook_event = crate::agent::hooks::HookEvent::BeforeToolCall {
                agent: self.cfg().agent.name.clone(),
                tool_name: name.to_string(),
            };
            let action = self.hooks().fire(&hook_event);
            self.hooks().fire_webhooks(&hook_event);
            if let crate::agent::hooks::HookAction::Block(reason) = action {
                return format!("Tool blocked by hook: {}", reason);
            }

            // 1. System tools (registry)
            let available = self.available_tool_names().await;
            let deps = crate::agent::tool_registry::ToolDeps::from_engine(self, &available);
            if let Some(result) = self.tool_registry.dispatch(name, &deps, arguments).await {
                return result;
            }

            // 2. YAML-defined tools — only VERIFIED may be called directly.
            if let Some(yaml_tool) = crate::tools::yaml_tools::find_yaml_tool(
                &self.cfg().workspace_dir,
                name,
            ).await {
                if yaml_tool.status == crate::tools::yaml_tools::ToolStatus::Draft {
                    return format!(
                        "Tool '{}' is in DRAFT status and cannot be called directly. \
                        Use tool_test(tool_name=\"{}\", test_params={{...}}) to test it, \
                        then tool_verify(tool_name=\"{}\") to promote it to verified.",
                        name, name, name
                    );
                }
                if yaml_tool.required_base && !self.cfg().agent.base {
                    return format!("Tool '{}' requires base agent.", name);
                }
                if name.starts_with("github_") {
                    let owner = arguments.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = arguments.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    if owner.is_empty() || repo_name.is_empty() {
                        return "GitHub tools require 'owner' and 'repo' parameters.".to_string();
                    }
                    match crate::db::github::check_repo_access(
                        &self.cfg().db, &self.cfg().agent.name, owner, repo_name,
                    ).await {
                        Ok(true) => {}
                        Ok(false) => return format!(
                            "Repository {}/{} is not in the allowed list for agent '{}'. \
                            Add it via POST /api/agents/{}/github/repos",
                            owner, repo_name, self.cfg().agent.name, self.cfg().agent.name
                        ),
                        Err(e) => return format!("Error checking repo access: {}", e),
                    }
                }
                if let Some(ref ca) = yaml_tool.channel_action.clone() {
                    return self.execute_yaml_channel_action(&yaml_tool, arguments, ca).await;
                }
                if CACHEABLE_SEARCH_TOOLS.contains(&name)
                    && let Some(q) = arguments.get("query").and_then(|v| v.as_str())
                    && let Some(cached) = self.check_search_cache(q).await
                {
                    return cached;
                }
                let resolver = self.make_resolver();
                let oauth_ctx = self.make_oauth_context();
                let client = if crate::tools::ssrf::is_internal_endpoint(&yaml_tool.endpoint) {
                    self.http_client()
                } else {
                    self.ssrf_http_client()
                };
                return match yaml_tool.execute_oauth(
                    arguments, client, Some(&resolver), oauth_ctx.as_ref(),
                ).await {
                    Ok(result) => {
                        if CACHEABLE_SEARCH_TOOLS.contains(&name)
                            && let Some(q) = arguments.get("query").and_then(|v| v.as_str())
                        {
                            self.store_search_cache(q, &result).await;
                        }
                        result
                    }
                    Err(e) => Self::format_tool_error(name, &e.to_string()),
                };
            }

            // 3. MCP tools
            if let Some(mcp) = self.mcp()
                && let Some(mcp_name) = mcp.find_mcp_for_tool(name).await
            {
                return match mcp.call_tool(&mcp_name, name, arguments).await {
                    Ok(result) => result,
                    Err(e) => Self::format_tool_error(name, &e.to_string()),
                };
            }

            // 4. External tool registry
            match self.cfg().tools.call(name, arguments).await {
                Ok(result) => serde_json::to_string(&result).unwrap_or_default(),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("tool not found") {
                        tracing::warn!(tool = %name, "LLM called non-existent tool");
                        format!(
                            "Error: tool '{}' does not exist. Use tool_list to see available tools.",
                            name
                        )
                    } else {
                        Self::format_tool_error(name, &msg)
                    }
                }
            }
        })
    }


    /// Filter tools based on per-agent allow/deny policy.
    /// Merge a cron-job tool policy override on top of the agent's base policy,
    /// then re-filter the already-filtered tool list.
    ///
    /// Logic:
    ///  - deny list is unioned (base deny ∪ override deny)
    ///  - allow list: if override has non-empty allow, restrict to those tools only (intersection with current list)
    ///
    /// `pub(crate)` so `engine::run::handle_isolated_via_pipeline` can
    /// apply the cron-side `BehaviourLayers::tool_policy_override` at
    /// the bootstrap boundary. Widening from `pub(super)` is safe — the
    /// function still lives inside `hydeclaw-core`.
    pub(crate) fn apply_tool_policy_override(
        &self,
        tools: Vec<ToolDefinition>,
        override_policy: &crate::config::AgentToolPolicy,
    ) -> Vec<ToolDefinition> {
        let base_deny = self.cfg().agent.tools.as_ref().map(|p| p.deny.as_slice());
        crate::agent::pipeline::dispatch::apply_tool_policy_override(tools, base_deny, override_policy)
    }

    pub(super) fn filter_tools_by_policy(&self, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
        crate::agent::pipeline::dispatch::filter_tools_by_policy(
            tools,
            self.cfg().agent.tools.as_ref(),
            self.cfg().memory_store.is_available(),
        )
    }

}
