//! Tool dispatch: execute_tool_call, execute_tool_call_inner, approval flow,
//! usage recording, and tool policy filtering.
//! Extracted from engine.rs for readability.

use super::*;
use crate::agent::context_builder::ContextBuilderDeps;
use crate::agent::pipeline::handlers as ph;
use crate::agent::pipeline::sandbox as ps;
use crate::agent::pipeline::subagent as psub;

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
            self.hooks().fire(&crate::agent::hooks::HookEvent::AfterToolResult {
                agent: self.cfg().agent.name.clone(),
                tool_name: name.to_string(),
                duration_ms: duration_ms as u64,
            });

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
            if let crate::agent::hooks::HookAction::Block(reason) = self.hooks().fire(&crate::agent::hooks::HookEvent::BeforeToolCall {
                agent: self.cfg().agent.name.clone(),
                tool_name: name.to_string(),
            }) {
                return format!("Tool blocked by hook: {}", reason);
            }

            // 1. Internal tools — match dispatch table
            if let Some(result) = match name {
                "workspace_write" => Some(ph::handle_workspace_write(
                    &self.cfg().workspace_dir,
                    &self.cfg().agent.name,
                    self.cfg().agent.base,
                    self.secrets().as_ref(),
                    self.cfg().app_config.uploads.signed_url_ttl_secs,
                    arguments,
                ).await),
                "workspace_read" => Some(ph::handle_workspace_read(&self.cfg().workspace_dir, &self.cfg().agent.name, arguments).await),
                "workspace_list" => Some(ph::handle_workspace_list(&self.cfg().workspace_dir, &self.cfg().agent.name, arguments).await),
                "workspace_edit" => Some(ph::handle_workspace_edit(
                    &self.cfg().workspace_dir,
                    &self.cfg().agent.name,
                    self.cfg().agent.base,
                    self.secrets().as_ref(),
                    self.cfg().app_config.uploads.signed_url_ttl_secs,
                    arguments,
                ).await),
                "workspace_delete" => Some(ph::handle_workspace_delete(&self.cfg().workspace_dir, &self.cfg().agent.name, arguments).await),
                "workspace_rename" => Some(ph::handle_workspace_rename(&self.cfg().workspace_dir, &self.cfg().agent.name, arguments).await),
                "memory" => Some(self.dispatch_memory_tool(arguments).await),
                "message" => Some(self.handle_message_action(arguments).await),
                "cron" => Some(self.handle_cron(arguments).await),
                "agent" => Some(crate::agent::pipeline::agent_tool::handle_agent_tool(
                    self.cfg().session_pools.as_ref(),
                    self.cfg().agent_map.as_ref(),
                    &self.cfg().db,
                    &self.cfg().agent.name,
                    arguments,
                ).await),
                "web_fetch" => {
                    let toolgate_url = self.cfg().app_config.toolgate_url.clone()
                        .unwrap_or_else(|| "http://localhost:9011".to_string());
                    Some(psub::handle_web_fetch(
                        self.http_client(),
                        &toolgate_url,
                        &self.cfg().app_config.gateway.listen,
                        arguments,
                    ).await)
                },
                "tool_create" => Some(ph::handle_tool_create(&self.cfg().workspace_dir, arguments).await),
                "tool_list" => Some(ph::handle_tool_list(&self.cfg().workspace_dir, arguments).await),
                "tool_test" => Some(ph::handle_tool_test(&self.cfg().workspace_dir, self.http_client(), self.ssrf_http_client(), self.secrets(), &self.cfg().agent.name, self.oauth().as_ref(), arguments).await),
                "tool_verify" => Some(ph::handle_tool_verify(&self.cfg().workspace_dir, arguments).await),
                "tool_disable" => Some(ph::handle_tool_disable(&self.cfg().workspace_dir, arguments).await),
                "skill" => Some(self.dispatch_skill_tool(arguments).await),
                "skill_use" => {
                    let available = self.available_tool_names().await;
                    Some(ph::handle_skill_use(&self.cfg().workspace_dir, self.cfg().agent.base, &available, arguments).await)
                }
                "tool_discover" => Some(ph::handle_tool_discover(&self.cfg().workspace_dir, self.ssrf_http_client(), arguments).await),
                "secret_set" => Some(ph::handle_secret_set(self.secrets(), &self.cfg().agent.name, self.cfg().agent.base, arguments).await),
                "session" => Some(self.dispatch_session_tool(arguments).await),
                "agents_list" => Some(crate::agent::pipeline::sessions::handle_agents_list(
                    self.cfg().agent_map.as_ref(),
                    self.cfg().session_pools.as_ref(),
                    &self.cfg().agent.name,
                    arguments,
                ).await),
                "browser_action" => Some(ph::handle_browser_action(self.http_client(), &crate::agent::pipeline::canvas::browser_renderer_url(), arguments).await),
                "code_exec" => Some(ps::handle_code_exec(
                    arguments,
                    &self.cfg().agent.name,
                    self.cfg().agent.base,
                    self.sandbox(),
                    &self.cfg().workspace_dir,
                    self.secrets().as_ref(),
                    self.cfg().app_config.uploads.signed_url_ttl_secs,
                ).await),
                "git" => Some(self.dispatch_git_tool(arguments).await),
                "canvas" => Some(crate::agent::pipeline::canvas::handle_canvas(&self.tex().canvas_state, &self.cfg().agent.name, self.state().ui_event_tx.as_ref(), self.http_client(), arguments).await),
                "rich_card" => Some(ph::handle_rich_card(arguments)),
                "process" => Some(self.dispatch_process_tool(arguments).await),
                _ => None,
            } {
                return result;
            }
            // 2. YAML-defined tools (workspace/tools/) — only VERIFIED may be called directly.
            // Draft tools are blocked here; they can only be invoked through tool_test.
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
                // GitHub repo access enforcement: tools starting with "github_" require allowed repo
                if name.starts_with("github_") {
                    let owner = arguments.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = arguments.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    if owner.is_empty() || repo_name.is_empty() {
                        return "GitHub tools require 'owner' and 'repo' parameters.".to_string();
                    }
                    match crate::db::github::check_repo_access(&self.cfg().db, &self.cfg().agent.name, owner, repo_name).await {
                        Ok(true) => {} // allowed
                        Ok(false) => {
                            return format!(
                                "Repository {}/{} is not in the allowed list for agent '{}'. \
                                Add it via POST /api/agents/{}/github/repos",
                                owner, repo_name, self.cfg().agent.name, self.cfg().agent.name
                            );
                        }
                        Err(e) => {
                            return format!("Error checking repo access: {}", e);
                        }
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
                // Internal endpoints (toolgate, searxng, browser-renderer) bypass SSRF filtering
                let client = if crate::tools::ssrf::is_internal_endpoint(&yaml_tool.endpoint) {
                    self.http_client()
                } else {
                    self.ssrf_http_client()
                };
                return match yaml_tool.execute_oauth(arguments, client, Some(&resolver), oauth_ctx.as_ref()).await {
                    Ok(result) => {
                        if CACHEABLE_SEARCH_TOOLS.contains(&name)
                            && let Some(q) = arguments.get("query").and_then(|v| v.as_str())
                        {
                            self.store_search_cache(q, &result).await;
                        }
                        result
                    },
                    Err(e) => Self::format_tool_error(name, &e.to_string()),
                };
            }

            // 3. MCP tools (via MCP)
            if let Some(mcp) = self.mcp()
                && let Some(mcp_name) = mcp.find_mcp_for_tool(name).await {
                    return match mcp.call_tool(&mcp_name, name, arguments).await {
                        Ok(result) => result,
                        Err(e) => Self::format_tool_error(name, &e.to_string()),
                    };
                }

            // 5. External tools via ToolRegistry (fallback)
            match self.cfg().tools.call(name, arguments).await {
                Ok(result) => serde_json::to_string(&result).unwrap_or_default(),
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("tool not found") {
                        tracing::warn!(tool = %name, "LLM called non-existent tool");
                        format!("Error: tool '{}' does not exist. Use tool_list to see available tools.", name)
                    } else {
                        Self::format_tool_error(name, &msg)
                    }
                }
            }
        })
    }


    /// Record LLM token usage to the database (fire-and-forget).
    pub(super) fn record_usage(&self, response: &hydeclaw_types::LlmResponse, session_id: Option<uuid::Uuid>) {
        if let Some(ref usage) = response.usage {
            let db = self.cfg().db.clone();
            let agent = self.cfg().agent.name.clone();
            let provider = response.provider.clone()
                .unwrap_or_else(|| self.cfg().provider.name().to_string());
            let model = response.model.clone().unwrap_or_default();
            let input = usage.input_tokens;
            let output = usage.output_tokens;
            tokio::spawn(async move {
                if let Err(e) = crate::db::usage::record_usage(
                    &db, &agent, &provider, &model, input, output, session_id,
                ).await {
                    tracing::debug!(error = %e, "failed to record usage");
                }
            });
        }
    }

    /// Filter tools based on per-agent allow/deny policy.
    /// Merge a cron-job tool policy override on top of the agent's base policy,
    /// then re-filter the already-filtered tool list.
    ///
    /// Logic:
    ///  - deny list is unioned (base deny ∪ override deny)
    ///  - allow list: if override has non-empty allow, restrict to those tools only (intersection with current list)
    pub(super) fn apply_tool_policy_override(
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

    // ── Dispatch helpers for tools with sub-action routing ──────────────

    async fn dispatch_memory_tool(&self, arguments: &serde_json::Value) -> String {
        use crate::agent::pipeline::memory as pipeline_memory;
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "search" => {
                let pinned_ids = self.tex().pinned_chunk_ids.lock().await.clone();
                pipeline_memory::handle_memory_search(
                    self.cfg().memory_store.as_ref(), &self.cfg().agent.name, &pinned_ids, arguments,
                ).await
            }
            "index" => pipeline_memory::handle_memory_index(
                self.cfg().memory_store.as_ref(), &self.cfg().agent.name, arguments,
            ).await,
            "reindex" => pipeline_memory::handle_memory_reindex(
                self.cfg().memory_store.as_ref(), &self.cfg().agent.name, &self.cfg().workspace_dir, arguments,
            ).await,
            "get" => pipeline_memory::handle_memory_get(self.cfg().memory_store.as_ref(), arguments).await,
            "delete" => pipeline_memory::handle_memory_delete(self.cfg().memory_store.as_ref(), arguments).await,
            "update" => {
                // Remap sub_action -> action for handle_memory_update compatibility
                let mut args = arguments.clone();
                if let Some(sa) = arguments.get("sub_action").cloned()
                    && let Some(obj) = args.as_object_mut() {
                        obj.insert("action".to_string(), sa);
                    }
                pipeline_memory::handle_memory_update(
                    &self.tex().memory_md_lock, &self.cfg().workspace_dir, &self.cfg().agent.name, &args,
                ).await
            }
            _ => format!("Error: unknown memory action '{}'. Use: search, index, reindex, get, delete, update.", action),
        }
    }

    async fn dispatch_skill_tool(&self, arguments: &serde_json::Value) -> String {
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "create" => ph::handle_skill_create(&self.cfg().workspace_dir, arguments).await,
            "update" => ph::handle_skill_create(&self.cfg().workspace_dir, arguments).await,
            "list" => {
                let available = self.available_tool_names().await;
                ph::handle_skill_list(&self.cfg().workspace_dir, self.cfg().agent.base, &available, arguments).await
            }
            _ => format!("Error: unknown skill action '{}'. Use: create, update, list.", action),
        }
    }

    async fn dispatch_session_tool(&self, arguments: &serde_json::Value) -> String {
        use crate::agent::pipeline::sessions;
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "list" => sessions::handle_sessions_list(&self.cfg().db, &self.cfg().agent.name, arguments).await,
            "history" => sessions::handle_sessions_history(&self.cfg().db, &self.cfg().agent.name, arguments).await,
            "search" => sessions::handle_session_search(&self.cfg().db, &self.cfg().agent.name, arguments).await,
            "context" => sessions::handle_session_context(&self.cfg().db, arguments).await,
            "send" => sessions::handle_session_send(self.state().channel_router.as_ref(), arguments).await,
            "export" => sessions::handle_session_export(&self.cfg().db, arguments).await,
            _ => format!("Error: unknown session action '{}'. Use: list, history, search, context, send, export.", action),
        }
    }

    async fn dispatch_process_tool(&self, arguments: &serde_json::Value) -> String {
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "start" => ps::handle_process_start(arguments, &self.cfg().agent.name, &self.tex().bg_processes).await,
            "status" => ps::handle_process_status(arguments, &self.tex().bg_processes).await,
            "logs" => ps::handle_process_logs(arguments, &self.tex().bg_processes).await,
            "kill" => ps::handle_process_kill(arguments, &self.tex().bg_processes).await,
            _ => format!("Error: unknown process action '{}'. Use: start, status, logs, kill.", action),
        }
    }

    async fn dispatch_git_tool(&self, arguments: &serde_json::Value) -> String {
        let action = arguments.get("action").and_then(|v| v.as_str()).unwrap_or("");

        // Clone is special — doesn't need existing git dir
        if action == "clone" {
            let url = match arguments.get("url").and_then(|v| v.as_str()).filter(|u| !u.is_empty()) {
                Some(u) => u.to_string(),
                None => return "Error: url parameter required.".to_string(),
            };
            // Reject URLs starting with '-' to prevent git option injection (RCE via --upload-pack etc.)
            if url.starts_with('-') {
                return "Error: URL must not start with '-'".to_string();
            }
            let url = if url.starts_with("https://github.com/") {
                url.replace("https://github.com/", "git@github.com:")
            } else { url };
            let dir_name = arguments.get("directory").and_then(|v| v.as_str()).filter(|d| !d.is_empty())
                .map(|d| d.to_string())
                .unwrap_or_else(|| {
                    url.rsplit('/').next().or_else(|| url.rsplit(':').next())
                        .unwrap_or("repo").trim_end_matches(".git").to_string()
                });
            let target = std::path::PathBuf::from(&self.cfg().workspace_dir).join(&dir_name);
            // No pre-existence check (TOCTOU race). Let git clone fail naturally
            // if the directory already exists — git reports a clear error message.
            let output = tokio::process::Command::new("git")
                .args(["clone", "--", &url, &target.to_string_lossy()])
                .output().await;
            return match output {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if o.status.success() { format!("Cloned {} into {}\n{}{}", url, dir_name, stdout, stderr) }
                    else { format!("git clone failed:\n{}{}", stdout, stderr) }
                }
                Err(e) => format!("Error running git clone: {}", e),
            };
        }

        // All other actions need a git working directory
        let git_dir = match arguments.get("directory").and_then(|v| v.as_str()).filter(|d| !d.is_empty()) {
            Some(sub) => {
                let p = std::path::PathBuf::from(&self.cfg().workspace_dir).join(sub);
                if !p.exists() || !p.is_dir() { return format!("Error: directory '{}' not found in workspace.", sub); }
                p.to_string_lossy().to_string()
            }
            None => {
                let ws = std::path::PathBuf::from(&self.cfg().workspace_dir);
                if !ws.join(".git").exists() {
                    let mut git_dirs = Vec::new();
                    if let Ok(mut entries) = tokio::fs::read_dir(&ws).await {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let p = entry.path();
                            if p.is_dir() && p.join(".git").exists()
                                && let Some(dn) = p.file_name().and_then(|n| n.to_str()) { git_dirs.push(dn.to_string()); }
                        }
                    }
                    if !git_dirs.is_empty() {
                        return format!("Error: workspace root is not a git repo. Use directory parameter. Found: {}", git_dirs.join(", "));
                    }
                    return "Error: no git repository found in workspace.".to_string();
                }
                ws.to_string_lossy().to_string()
            }
        };

        match action {
            "commit" => {
                let message = arguments.get("message").and_then(|v| v.as_str()).unwrap_or("chore: update files");
                match tokio::process::Command::new("git").args(["commit", "-am", message]).current_dir(&git_dir).output().await {
                    Ok(o) => { let s = String::from_utf8_lossy(&o.stdout); let e = String::from_utf8_lossy(&o.stderr);
                        if o.status.success() { s.to_string() } else { format!("git commit failed: {}{}", s, e) } }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "log" => {
                let limit = arguments.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
                let oneline = arguments.get("oneline").and_then(|v| v.as_bool()).unwrap_or(true);
                let mut args = vec!["log".to_string(), format!("-{}", limit)];
                if oneline { args.push("--oneline".to_string()); }
                else { args.push("--format=%h %ad %an: %s".to_string()); args.push("--date=short".to_string()); }
                match tokio::process::Command::new("git").args(&args).current_dir(&git_dir).output().await {
                    Ok(o) => { let out = String::from_utf8_lossy(&o.stdout).to_string();
                        if out.is_empty() { "No commits found.".to_string() } else { out } }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "add" => {
                let files: Vec<String> = arguments.get("files").and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|f| f.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
                if files.is_empty() { return "Error: files parameter required.".to_string(); }
                let mut args = vec!["add".to_string()]; args.extend(files);
                match tokio::process::Command::new("git").args(&args).current_dir(&git_dir).output().await {
                    Ok(o) => if o.status.success() { let s = String::from_utf8_lossy(&o.stdout);
                        if s.is_empty() { "Files staged.".to_string() } else { s.to_string() } }
                        else { format!("git add failed: {}", String::from_utf8_lossy(&o.stderr)) }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "branch" => {
                let branch_act = arguments.get("branch_action").and_then(|v| v.as_str()).unwrap_or("list");
                let branch_name = arguments.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args: Vec<&str> = match branch_act {
                    "list" => vec!["branch", "-a"],
                    "create" => { if branch_name.is_empty() { return "Error: name required.".to_string(); } vec!["checkout", "-b", branch_name] }
                    "switch" => { if branch_name.is_empty() { return "Error: name required.".to_string(); } vec!["checkout", branch_name] }
                    "delete" => { if branch_name.is_empty() { return "Error: name required.".to_string(); } vec!["branch", "-d", branch_name] }
                    _ => return format!("Error: unknown branch_action '{}'.", branch_act),
                };
                match tokio::process::Command::new("git").args(&args).current_dir(&git_dir).output().await {
                    Ok(o) => { let mut out = String::from_utf8_lossy(&o.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&o.stderr); if !stderr.is_empty() { out.push_str(&stderr); }
                        if out.is_empty() { format!("Exit code: {}", o.status.code().unwrap_or(-1)) } else { out } }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "status" | "diff" | "push" | "pull" => {
                match tokio::process::Command::new("git").args([action]).current_dir(&git_dir).output().await {
                    Ok(o) => { let mut out = String::from_utf8_lossy(&o.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        if !stderr.is_empty() { out.push_str("\n--- stderr ---\n"); out.push_str(&stderr); }
                        if out.is_empty() { format!("Exit code: {}", o.status.code().unwrap_or(-1)) } else { out } }
                    Err(e) => format!("Error running git {}: {}", action, e),
                }
            }
            _ => format!("Error: unknown git action '{}'. Use: status, diff, log, commit, add, push, pull, branch, clone.", action),
        }
    }
}
