use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::agent::lsp::manager::{LspAction, LspManager};
use crate::agent::lsp::servers::server_for_path;
use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

// ── LSP auto-diagnostics helpers ──────────────────────────────────────────────

/// Return `true` when LSP diagnostics should be collected for `file`.
///
/// Checks only whether a language server is registered for the file extension.
/// The `lsp_manager.is_some()` manager-present gate is kept at each call site
/// (inside `if let Some(mgr) = deps.cfg.lsp_manager`) — zero overhead when LSP
/// is disabled.
pub(crate) fn should_diagnose(file: &str) -> bool {
    server_for_path(file).is_some()
}

/// Append LSP diagnostics for each of `files` to `base_result`.
///
/// Best-effort: any individual diagnostic failure is silently skipped.
/// If no files are diagnosable the result is returned unchanged.
pub(crate) async fn append_diagnostics(
    mgr: &Arc<LspManager>,
    agent_name: &str,
    workspace_dir: &str,
    files: &[&str],
    mut result: String,
) -> String {
    for file in files {
        if !should_diagnose(file) {
            continue;
        }
        if let Ok(text) = mgr.op(agent_name, workspace_dir, file, LspAction::Diagnostics).await {
            if text.is_empty() || text == "No diagnostics." {
                result.push_str("\n\nNo diagnostics.");
            } else {
                result.push_str("\n\nDiagnostics:\n");
                result.push_str(&text);
            }
        }
        // best-effort: Err silently ignored
    }
    result
}

/// Best-effort снапшот scope перед мутацией. Любая ошибка → warn, не блокирует.
pub(crate) async fn maybe_checkpoint(
    mgr: &Option<std::sync::Arc<crate::agent::checkpoint_manager::CheckpointManager>>,
    agent_name: &str,
    workspace_dir: &str,
) {
    if let Some(cm) = mgr
        && let Err(e) = cm.ensure_checkpoint(agent_name, workspace_dir).await
    {
        tracing::warn!(agent = %agent_name, error = %e, "checkpoint ensure failed (non-fatal)");
    }
}

pub struct WorkspaceWriteHandler;
pub struct WorkspaceReadHandler;
pub struct WorkspaceListHandler;
pub struct WorkspaceEditHandler;
pub struct WorkspaceDeleteHandler;
pub struct WorkspaceRenameHandler;

#[async_trait]
impl SystemToolHandler for WorkspaceWriteHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        maybe_checkpoint(&deps.cfg.checkpoint_manager, deps.agent_name, deps.workspace_dir).await;
        let result = ph::handle_workspace_write(
            deps.workspace_dir,
            deps.agent_name,
            deps.agent_base,
            deps.secrets.as_ref(),
            deps.signed_url_ttl_secs,
            args,
        )
        .await;
        let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(mgr) = &deps.cfg.lsp_manager
            && should_diagnose(filename)
        {
            return append_diagnostics(mgr, deps.agent_name, deps.workspace_dir, &[filename], result).await;
        }
        result
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceReadHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_read(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceListHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        ph::handle_workspace_list(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceEditHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        maybe_checkpoint(&deps.cfg.checkpoint_manager, deps.agent_name, deps.workspace_dir).await;
        let result = ph::handle_workspace_edit(
            deps.workspace_dir,
            deps.agent_name,
            deps.agent_base,
            deps.secrets.as_ref(),
            deps.signed_url_ttl_secs,
            args,
        )
        .await;
        let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(mgr) = &deps.cfg.lsp_manager
            && should_diagnose(filename)
        {
            return append_diagnostics(mgr, deps.agent_name, deps.workspace_dir, &[filename], result).await;
        }
        result
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceDeleteHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        maybe_checkpoint(&deps.cfg.checkpoint_manager, deps.agent_name, deps.workspace_dir).await;
        ph::handle_workspace_delete(deps.workspace_dir, deps.agent_name, args).await
    }
}

#[async_trait]
impl SystemToolHandler for WorkspaceRenameHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        maybe_checkpoint(&deps.cfg.checkpoint_manager, deps.agent_name, deps.workspace_dir).await;
        ph::handle_workspace_rename(deps.workspace_dir, deps.agent_name, args).await
    }
}

pub struct ApplyPatchHandler;

#[async_trait]
impl SystemToolHandler for ApplyPatchHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        maybe_checkpoint(&deps.cfg.checkpoint_manager, deps.agent_name, deps.workspace_dir).await;
        let result =
            ph::handle_apply_patch(deps.workspace_dir, deps.agent_name, deps.agent_base, args)
                .await;
        let patch = args.get("patch").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(mgr) = &deps.cfg.lsp_manager
            && let Ok(ops) = crate::agent::v4a_patch::parse_patch(patch)
        {
            let files: Vec<&str> = ops
                .iter()
                .map(|op| match op {
                    crate::agent::v4a_patch::FileOp::Update { path, .. } => path.as_str(),
                    crate::agent::v4a_patch::FileOp::Add { path, .. } => path.as_str(),
                })
                .filter(|f| should_diagnose(f))
                .collect();
            if !files.is_empty() {
                return append_diagnostics(mgr, deps.agent_name, deps.workspace_dir, &files, result)
                    .await;
            }
        }
        result
    }
}

pub struct LspHandler;

#[async_trait]
impl SystemToolHandler for LspHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        // Checkpoint before rename (the only mutating action); other actions are read-only
        // but maybe_checkpoint is a no-op when there's nothing to snap — safe to call always.
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if action == "rename" {
            maybe_checkpoint(&deps.cfg.checkpoint_manager, deps.agent_name, deps.workspace_dir)
                .await;
        }
        ph::handle_lsp(
            deps.cfg.lsp_manager.as_ref(),
            deps.workspace_dir,
            deps.agent_name,
            deps.agent_base,
            args,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_workspace_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(WorkspaceWriteHandler);
        assert_impl(WorkspaceReadHandler);
        assert_impl(WorkspaceListHandler);
        assert_impl(WorkspaceEditHandler);
        assert_impl(WorkspaceDeleteHandler);
        assert_impl(WorkspaceRenameHandler);
    }

    // ── should_diagnose unit tests ─────────────────────────────────────────────

    #[test]
    fn should_diagnose_py_true() {
        // .py has a registered language server → true
        assert!(should_diagnose("script.py"));
    }

    #[test]
    fn should_diagnose_md_false() {
        // .md has no language server → false
        assert!(!should_diagnose("notes.md"));
    }

    #[test]
    fn should_diagnose_rs_false() {
        // .rs is v2 (not yet registered) → false
        assert!(!should_diagnose("lib.rs"));
    }

    #[test]
    fn should_diagnose_no_ext_false() {
        // no extension → false
        assert!(!should_diagnose("noext"));
    }
}

#[cfg(test)]
mod cp_tests {
    use super::*;

    #[tokio::test]
    async fn maybe_checkpoint_snaps_then_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let ws = tmp.path().join("ws");
        let cfg = crate::config::CheckpointConfig {
            store_path: store.to_str().unwrap().to_string(),
            ..Default::default()
        };
        let mgr = std::sync::Arc::new(
            crate::agent::checkpoint_manager::CheckpointManager::new(cfg)
        );
        // подготовить scope
        let p = ws.join("agents").join("Agent").join("x.md");
        tokio::fs::create_dir_all(p.parent().unwrap()).await.unwrap();
        tokio::fs::write(&p, "v1").await.unwrap();

        maybe_checkpoint(&Some(mgr.clone()), "Agent", ws.to_str().unwrap()).await;
        assert!(store.join("refs/checkpoints/Agent/1").exists());

        // None-менеджер — не паникует
        maybe_checkpoint(&None, "Agent", ws.to_str().unwrap()).await;
    }
}
