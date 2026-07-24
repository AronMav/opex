//! Communication-layer system tool handlers: message, cron, git, canvas,
//! rich_card, browser_action, process.
//!
//! Each handler delegates to the same free functions used by `engine_dispatch.rs`,
//! building a `CommandContext` where needed.

use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::handlers as ph;
use crate::agent::pipeline::sandbox as ps;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

// ── message ──────────────────────────────────────────────────────────────────

pub struct MessageHandler;

#[async_trait]
impl SystemToolHandler for MessageHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: deps.cfg,
            state: deps.state,
            tex: deps.tex,
            subagent_depth: 0,
        };
        crate::agent::pipeline::channel_actions::handle_message_action(&ctx, args).await
    }
}

// ── cron ─────────────────────────────────────────────────────────────────────

pub struct CronHandler;

#[async_trait]
impl SystemToolHandler for CronHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: deps.cfg,
            state: deps.state,
            tex: deps.tex,
            subagent_depth: 0,
        };
        crate::agent::pipeline::cron::handle_cron(&ctx, args).await
    }
}

// ── git ───────────────────────────────────────────────────────────────────────

pub struct GitToolHandler;

#[async_trait]
impl SystemToolHandler for GitToolHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

        // Clone is special — doesn't need existing git dir
        if action == "clone" {
            let url = match args.get("url").and_then(|v| v.as_str()).filter(|u| !u.is_empty()) {
                Some(u) => u.to_string(),
                None => return "Error: url parameter required.".to_string(),
            };
            // Reject URLs starting with '-' to prevent git option injection (RCE via --upload-pack etc.)
            if url.starts_with('-') {
                return "Error: URL must not start with '-'".to_string();
            }
            let url = if url.starts_with("https://github.com/") {
                url.replace("https://github.com/", "git@github.com:")
            } else {
                url
            };
            let dir_name = args
                .get("directory")
                .and_then(|v| v.as_str())
                .filter(|d| !d.is_empty())
                .map(|d| d.to_string())
                .unwrap_or_else(|| {
                    url.rsplit('/')
                        .next()
                        .or_else(|| url.rsplit(':').next())
                        .unwrap_or("repo")
                        .trim_end_matches(".git")
                        .to_string()
                });
            let target = std::path::PathBuf::from(deps.workspace_dir).join(&dir_name);
            // No pre-existence check (TOCTOU race). Let git clone fail naturally
            // if the directory already exists — git reports a clear error message.
            let output = tokio::process::Command::new("git")
                .args(["clone", "--", &url, &target.to_string_lossy()])
                .output()
                .await;
            return match output {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if o.status.success() {
                        format!("Cloned {} into {}\n{}{}", url, dir_name, stdout, stderr)
                    } else {
                        format!("git clone failed:\n{}{}", stdout, stderr)
                    }
                }
                Err(e) => format!("Error running git clone: {}", e),
            };
        }

        // All other actions need a git working directory
        let git_dir = match args
            .get("directory")
            .and_then(|v| v.as_str())
            .filter(|d| !d.is_empty())
        {
            Some(sub) => {
                let p = std::path::PathBuf::from(deps.workspace_dir).join(sub);
                if !p.exists() || !p.is_dir() {
                    return format!("Error: directory '{}' not found in workspace.", sub);
                }
                p.to_string_lossy().to_string()
            }
            None => {
                let ws = std::path::PathBuf::from(deps.workspace_dir);
                if !ws.join(".git").exists() {
                    let mut git_dirs = Vec::new();
                    if let Ok(mut entries) = tokio::fs::read_dir(&ws).await {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let p = entry.path();
                            if p.is_dir()
                                && p.join(".git").exists()
                                && let Some(dn) = p.file_name().and_then(|n| n.to_str())
                            {
                                git_dirs.push(dn.to_string());
                            }
                        }
                    }
                    if !git_dirs.is_empty() {
                        return format!(
                            "Error: workspace root is not a git repo. Use directory parameter. Found: {}",
                            git_dirs.join(", ")
                        );
                    }
                    return "Error: no git repository found in workspace.".to_string();
                }
                ws.to_string_lossy().to_string()
            }
        };

        match action {
            "commit" => {
                let message = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("chore: update files");
                match tokio::process::Command::new("git")
                    .args(["commit", "-am", message])
                    .current_dir(&git_dir)
                    .output()
                    .await
                {
                    Ok(o) => {
                        let s = String::from_utf8_lossy(&o.stdout);
                        let e = String::from_utf8_lossy(&o.stderr);
                        if o.status.success() {
                            s.to_string()
                        } else {
                            format!("git commit failed: {}{}", s, e)
                        }
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "log" => {
                let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
                let oneline = args.get("oneline").and_then(|v| v.as_bool()).unwrap_or(true);
                let mut git_args = vec!["log".to_string(), format!("-{}", limit)];
                if oneline {
                    git_args.push("--oneline".to_string());
                } else {
                    git_args.push("--format=%h %ad %an: %s".to_string());
                    git_args.push("--date=short".to_string());
                }
                match tokio::process::Command::new("git")
                    .args(&git_args)
                    .current_dir(&git_dir)
                    .output()
                    .await
                {
                    Ok(o) => {
                        let out = String::from_utf8_lossy(&o.stdout).to_string();
                        if out.is_empty() {
                            "No commits found.".to_string()
                        } else {
                            out
                        }
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "add" => {
                let files: Vec<String> = args
                    .get("files")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if files.is_empty() {
                    return "Error: files parameter required.".to_string();
                }
                let mut git_args = vec!["add".to_string()];
                git_args.extend(files);
                match tokio::process::Command::new("git")
                    .args(&git_args)
                    .current_dir(&git_dir)
                    .output()
                    .await
                {
                    Ok(o) => {
                        if o.status.success() {
                            let s = String::from_utf8_lossy(&o.stdout);
                            if s.is_empty() {
                                "Files staged.".to_string()
                            } else {
                                s.to_string()
                            }
                        } else {
                            format!("git add failed: {}", String::from_utf8_lossy(&o.stderr))
                        }
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "branch" => {
                let branch_act = args
                    .get("branch_action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("list");
                let branch_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let git_args: Vec<&str> = match branch_act {
                    "list" => vec!["branch", "-a"],
                    "create" => {
                        if branch_name.is_empty() {
                            return "Error: name required.".to_string();
                        }
                        vec!["checkout", "-b", branch_name]
                    }
                    "switch" => {
                        if branch_name.is_empty() {
                            return "Error: name required.".to_string();
                        }
                        vec!["checkout", branch_name]
                    }
                    "delete" => {
                        if branch_name.is_empty() {
                            return "Error: name required.".to_string();
                        }
                        vec!["branch", "-d", branch_name]
                    }
                    _ => return format!("Error: unknown branch_action '{}'.", branch_act),
                };
                match tokio::process::Command::new("git")
                    .args(&git_args)
                    .current_dir(&git_dir)
                    .output()
                    .await
                {
                    Ok(o) => {
                        let mut out = String::from_utf8_lossy(&o.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        if !stderr.is_empty() {
                            out.push_str(&stderr);
                        }
                        if out.is_empty() {
                            format!("Exit code: {}", o.status.code().unwrap_or(-1))
                        } else {
                            out
                        }
                    }
                    Err(e) => format!("Error: {}", e),
                }
            }
            "status" | "diff" | "push" | "pull" => {
                let git_timeout = std::time::Duration::from_secs(60);
                match tokio::time::timeout(
                    git_timeout,
                    tokio::process::Command::new("git")
                        .args([action])
                        .current_dir(&git_dir)
                        .output(),
                )
                .await
                {
                    Ok(Ok(o)) => {
                        let mut out = String::from_utf8_lossy(&o.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        if !stderr.is_empty() {
                            out.push_str("\n--- stderr ---\n");
                            out.push_str(&stderr);
                        }
                        if out.is_empty() {
                            format!("Exit code: {}", o.status.code().unwrap_or(-1))
                        } else {
                            out
                        }
                    }
                    Ok(Err(e)) => format!("Error running git {}: {}", action, e),
                    Err(_) => format!("Error: git {} timed out (60s)", action),
                }
            }
            _ => format!(
                "Error: unknown git action '{}'. Use: status, diff, log, commit, add, push, pull, branch, clone.",
                action
            ),
        }
    }
}

// ── canvas ────────────────────────────────────────────────────────────────────

pub struct CanvasHandler;

#[async_trait]
impl SystemToolHandler for CanvasHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        crate::agent::pipeline::canvas::handle_canvas(
            &deps.tex.canvas_state,
            deps.agent_name,
            deps.ui_event_tx,
            deps.http_client,
            args,
        )
        .await
    }
}

// ── rich_card ─────────────────────────────────────────────────────────────────

pub struct RichCardHandler;

#[async_trait]
impl SystemToolHandler for RichCardHandler {
    async fn handle(&self, _deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_rich_card(args)
    }
}

// ── send_media ───────────────────────────────────────────────────────────────

pub struct SendMediaHandler;

#[async_trait]
impl SystemToolHandler for SendMediaHandler {
    async fn handle(&self, _deps: ToolDeps<'_>, args: &Value) -> String {
        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => return "Error: 'url' is required.".to_string(),
        };
        if !url.starts_with("/api/uploads/") && !url.starts_with("/uploads/") {
            return "Error: url must be an internal /api/uploads/ path.".to_string();
        }
        let media_type = args
            .get("media_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mut marker = serde_json::json!({
            "url": url,
            "mediaType": media_type,
        });
        if let Some(fname) = args
            .get("filename")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            marker["filename"] = serde_json::json!(fname);
        }
        format!(
            "{}{}\nMedia displayed inline.",
            crate::agent::engine::FILE_PREFIX,
            marker,
        )
    }
}

// ── browser_action ────────────────────────────────────────────────────────────

pub struct BrowserActionHandler;

#[async_trait]
impl SystemToolHandler for BrowserActionHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        if let Some(u) = args.get("url").and_then(|v| v.as_str())
            && crate::tools::url_policy::url_blocked(u, &deps.cfg.app_config.security.blocked_domains)
        {
            return format!("⛔ blocked by domain policy: {u}");
        }
        ph::handle_browser_action(
            deps.http_client,
            &crate::agent::pipeline::canvas::browser_renderer_url(),
            args,
        )
        .await
    }
}

// ── process ───────────────────────────────────────────────────────────────────

pub struct ProcessHandler;

#[async_trait]
impl SystemToolHandler for ProcessHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "start" => {
                ps::handle_process_start(args, deps.agent_name, &deps.tex.bg_processes).await
            }
            "status" => ps::handle_process_status(args, &deps.tex.bg_processes).await,
            "logs" => ps::handle_process_logs(args, &deps.tex.bg_processes).await,
            "kill" => ps::handle_process_kill(args, &deps.tex.bg_processes).await,
            _ => format!(
                "Error: unknown process action '{}'. Use: start, status, logs, kill.",
                action
            ),
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_comms_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(MessageHandler);
        assert_impl(CronHandler);
        assert_impl(GitToolHandler);
        assert_impl(CanvasHandler);
        assert_impl(RichCardHandler);
        assert_impl(BrowserActionHandler);
        assert_impl(ProcessHandler);
    }

    #[test]
    fn git_unknown_action_error_message() {
        let s = "Error: unknown git action 'foo'. Use: status, diff, log, commit, add, push, pull, branch, clone.";
        assert!(s.contains("foo"));
    }

    #[test]
    fn process_unknown_action_error_message() {
        let s = "Error: unknown process action 'foo'. Use: start, status, logs, kill.";
        assert!(s.contains("foo"));
    }
}
