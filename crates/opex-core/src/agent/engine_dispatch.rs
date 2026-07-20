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

            // Hook: AfterToolResult — sync notification + async decision (transform result).
            let hook_event = crate::agent::hooks::HookEvent::AfterToolResult {
                agent: self.cfg().agent.name.clone(),
                tool_name: name.to_string(),
                duration_ms: duration_ms as u64,
            };
            self.hooks().fire(&hook_event);
            self.hooks().fire_webhooks(&hook_event);
            let decision = self.hooks().fire_decision(
                &hook_event,
                serde_json::json!({ "result": result }),
            ).await;
            let result = if let crate::agent::hooks::HookDecision::TransformResult(s) = decision {
                self.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                    agent_name: self.cfg().agent.name.clone(),
                    session_id: None,
                    event_type: "AfterToolResult".into(),
                    action: "TransformResult".into(),
                    detail: None,
                });
                s
            } else {
                result
            };

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
                    agent_name: self.cfg().agent.name.clone(),
                    tool_name: name.to_string(),
                    success: !is_error,
                    duration_ms,
                    error: error_msg,
                });
            }

            result
        })
    }

    /// Codemode (tools-as-code) dispatch entry point. Routes a sandbox
    /// tool-call through the SAME `execute_tool_call` pipeline as the LLM loop
    /// so that `BeforeToolCall`/`AfterToolResult` hooks + decision-webhooks
    /// (Block / ModifyArgs / TransformResult), audit-log, latency metrics, and
    /// tool-quality records ALL apply — closing the gap where codemode bypassed
    /// them by calling the tool registry directly (SEC review 2026-07-06, H1/L3).
    ///
    /// Approval is still enforced *before* this call in the sandbox handler
    /// (codemode is non-interactive, so approval-required tools are rejected
    /// outright rather than reaching here). The caller must have already
    /// verified the tool is in the agent's policy-filtered available set.
    pub(crate) async fn codemode_execute_tool(&self, name: &str, arguments: &serde_json::Value) -> String {
        self.execute_tool_call(name, arguments).await
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

            // Hook: BeforeToolCall — sync phase (closures)
            let hook_event = crate::agent::hooks::HookEvent::BeforeToolCall {
                agent: self.cfg().agent.name.clone(),
                tool_name: name.to_string(),
            };
            let action = self.hooks().fire(&hook_event);
            self.hooks().fire_webhooks(&hook_event);
            if let crate::agent::hooks::HookAction::Block(reason) = action {
                return format!("Tool blocked by hook: {}", reason);
            }

            // Hook: BeforeToolCall — async decision phase (webhooks).
            // ModifyArgs is applied in-place via shadowed `arguments` binding —
            // no recursive execute_tool_call call, so hooks cannot re-fire.
            let decision = self.hooks().fire_decision(
                &hook_event,
                serde_json::json!({ "tool_input": arguments }),
            ).await;
            let modified_holder;
            let arguments: &serde_json::Value = match decision {
                crate::agent::hooks::HookDecision::Block(reason) => {
                    self.cfg().audit_queue.send(
                        crate::db::audit_queue::AuditEvent::HookDecision {
                            agent_name: self.cfg().agent.name.clone(),
                            session_id: None,
                            event_type: "BeforeToolCall".into(),
                            action: "Block".into(),
                            detail: Some(reason.chars().take(512).collect()),
                        },
                    );
                    return format!("Tool blocked by hook: {}", reason);
                }
                crate::agent::hooks::HookDecision::ModifyArgs(mut v) => {
                    // Preserve internal _context (mirrors ApprovedWithModifiedArgs rebind).
                    if let Some(ctx) = arguments.get("_context")
                        && let Some(obj) = v.as_object_mut()
                    {
                        obj.insert("_context".to_string(), ctx.clone());
                    }
                    self.cfg().audit_queue.send(
                        crate::db::audit_queue::AuditEvent::HookDecision {
                            agent_name: self.cfg().agent.name.clone(),
                            session_id: None,
                            event_type: "BeforeToolCall".into(),
                            action: "ModifyArgs".into(),
                            detail: None,
                        },
                    );
                    modified_holder = v;
                    &modified_holder
                }
                _ => arguments,
            };

            // 0. Reject malformed/invalid tool names before any dispatch or
            // audit so a corrupted tool name (e.g. a `__file__:` marker leaked
            // into the name field) cannot pollute the tool-quality log or
            // reach filesystem/HTTP dispatch.
            if !crate::agent::dispatcher::lookup::is_valid_tool_name(name) {
                return format!(
                    "Error: tool name '{}' is not a valid identifier; \
                     use tool_use(action='search') to find the correct tool name",
                    name
                );
            }

            // 1. System tools (registry)
            let available = self.available_tool_names().await;
            let dispatch_session_id = arguments
                .get("_context")
                .and_then(|c| c.get("session_id"))
                .and_then(|s| s.as_str())
                .and_then(|s| Uuid::parse_str(s).ok());
            let deps = crate::agent::tool_registry::ToolDeps::from_engine(
                self,
                &available,
                dispatch_session_id,
            );
            if let Some(result) = self.tool_registry.dispatch(name, &deps, arguments).await {
                return result;
            }

            // 2. YAML-defined tools (capability-names take priority over files).
            if let Some(yaml_tool) = crate::agent::capability_tools::resolve_tool(
                &self.cfg().workspace_dir,
                &self.cfg().profile_slots,
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

                // YAML tool response cache (pre-execution lookup). Skipped
                // when the tool has channel_action (binary response routed
                // to a channel, not returned to the LLM) or pagination
                // (multi-page fetch — caching one page is wrong).
                let cache_key = match &yaml_tool.cache {
                    Some(cfg)
                        if yaml_tool.channel_action.is_none()
                            && yaml_tool.pagination.is_none() =>
                    {
                        Some(crate::tools::yaml_tools::build_cache_key(
                            &self.cfg().agent.name,
                            &yaml_tool.name,
                            &yaml_tool.method,
                            &yaml_tool.endpoint,
                            arguments,
                            &cfg.key_params,
                        ))
                    }
                    _ => None,
                };
                if let Some(ref key) = cache_key
                    && let Some(body) = self.cfg().tool_exec_ctx.get_cached(key).await
                {
                    tracing::debug!(tool = %yaml_tool.name, "yaml tool cache hit");
                    return body;
                }

                // Domain blocklist for agent-supplied URLs in YAML tools (e.g. the
                // `browser` / `screenshot_web` aliases that reach internal renderers
                // via the internal-endpoint path, bypassing the system
                // web_fetch/browser_action handler check).
                if let Some(u) = arguments.get("url").and_then(|v| v.as_str())
                    && (u.starts_with("http://") || u.starts_with("https://"))
                    && crate::tools::url_policy::url_blocked(
                        u,
                        &self.cfg().app_config.security.blocked_domains,
                    )
                {
                    return format!("⛔ blocked by domain policy: {u}");
                }

                // F008: literal-IP SSRF gate. reqwest connects straight to a
                // literal IP written into the URL without ever invoking the
                // DNS resolver, so the SSRF/LAN client's DNS filter alone never
                // sees `http://169.254.169.254/…`. Validate the endpoint inline
                // (skips trusted internal services; permits RFC1918 only for
                // allow_private_endpoint tools) before dispatch.
                let ssrf_check = if yaml_tool.allow_private_endpoint {
                    crate::tools::ssrf::validate_lan_endpoint(&yaml_tool.endpoint)
                } else {
                    crate::tools::ssrf::validate_outbound_endpoint(&yaml_tool.endpoint)
                };
                if let Err(e) = ssrf_check {
                    return format!("⛔ blocked by SSRF policy: {e}");
                }

                let resolver = self.make_resolver();
                let oauth_ctx = self.make_oauth_context();
                let lan_client;
                let client = if crate::tools::ssrf::is_internal_endpoint(&yaml_tool.endpoint) {
                    self.http_client()
                } else if yaml_tool.allow_private_endpoint {
                    // Admin-authored tool explicitly allowed to reach a private
                    // LAN/tunnel host (still blocks loopback/metadata/CGNAT).
                    lan_client =
                        crate::tools::ssrf::lan_http_client(std::time::Duration::from_secs(30));
                    &lan_client
                } else {
                    self.ssrf_http_client()
                };
                // Capability tools whose provider is profile-routed carry the
                // ordered profile-slot chain so the downstream service can
                // retry across providers in the agent's configured order:
                // `search_web` → websearch (consumed by toolgate search.py),
                // `analyze_image` → vision (consumed by core
                // api_vision_analyze, which forwards one X-Opex-Provider per
                // attempt to toolgate). Every other yaml tool keeps the empty
                // header slice it always had (mirrors `execute_oauth`'s `&[]`).
                let mut injected_headers: Vec<(String, String)> = Vec::new();
                let chain_capability = match name {
                    "search_web" => Some("websearch"),
                    "analyze_image" => Some("vision"),
                    _ => None,
                };
                if let Some(cap) = chain_capability
                    && let Some(h) = slot_chain_header(&self.cfg().profile_slots, cap)
                {
                    injected_headers.push(h);
                }
                return match yaml_tool.execute_with_ctx(
                    arguments, client, Some(&resolver), oauth_ctx.as_ref(), &injected_headers,
                ).await {
                    Ok(result) => {
                        if CACHEABLE_SEARCH_TOOLS.contains(&name)
                            && let Some(q) = arguments.get("query").and_then(|v| v.as_str())
                        {
                            self.store_search_cache(q, &result).await;
                        }
                        if let (Some(key), Some(cfg)) =
                            (cache_key.as_ref(), yaml_tool.cache.as_ref())
                        {
                            self.cfg()
                                .tool_exec_ctx
                                .set_cached(key, &result, cfg.ttl)
                                .await;
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
    /// function still lives inside `opex-core`.
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

/// Builds the `X-Opex-Providers` header carrying the ordered profile-slot
/// provider chain for a capability (e.g. websearch `"searxng,ollama,brave"`,
/// vision `"ollama-cloud-vision,mimo-vision"`), joined by comma, so the
/// downstream service can retry across providers in the agent's configured
/// order. Returns `None` when the slot is missing or empty — in that case the
/// downstream falls back to its own default provider selection.
fn slot_chain_header(
    slots: &crate::db::profiles::Slots,
    capability: &str,
) -> Option<(String, String)> {
    let chain = slots
        .get(capability)?
        .iter()
        .map(|e| e.provider.as_str())
        .collect::<Vec<_>>()
        .join(",");
    if chain.is_empty() {
        None
    } else {
        Some(("X-Opex-Providers".to_string(), chain))
    }
}

#[cfg(test)]
mod slot_chain_header_tests {
    use super::slot_chain_header;
    use crate::db::profiles::SlotEntry;
    use std::collections::HashMap;

    fn entry(provider: &str) -> SlotEntry {
        SlotEntry {
            provider: provider.to_string(),
            model: None,
            voice: None,
        }
    }

    #[test]
    fn joins_multiple_providers_in_order() {
        let mut slots = HashMap::new();
        slots.insert("websearch".to_string(), vec![entry("a"), entry("b")]);
        assert_eq!(
            slot_chain_header(&slots, "websearch"),
            Some(("X-Opex-Providers".to_string(), "a,b".to_string()))
        );
    }

    #[test]
    fn vision_chain_reads_vision_slot_only() {
        let mut slots = HashMap::new();
        slots.insert("websearch".to_string(), vec![entry("ws")]);
        slots.insert("vision".to_string(), vec![entry("ollama-v"), entry("mimo-v")]);
        assert_eq!(
            slot_chain_header(&slots, "vision"),
            Some(("X-Opex-Providers".to_string(), "ollama-v,mimo-v".to_string()))
        );
        // websearch chain untouched by the vision entry
        assert_eq!(
            slot_chain_header(&slots, "websearch"),
            Some(("X-Opex-Providers".to_string(), "ws".to_string()))
        );
    }

    #[test]
    fn missing_slot_returns_none() {
        let slots: HashMap<String, Vec<SlotEntry>> = HashMap::new();
        assert_eq!(slot_chain_header(&slots, "websearch"), None);
        assert_eq!(slot_chain_header(&slots, "vision"), None);
    }

    #[test]
    fn empty_slot_returns_none() {
        let mut slots = HashMap::new();
        slots.insert("websearch".to_string(), vec![]);
        assert_eq!(slot_chain_header(&slots, "websearch"), None);
    }
}
