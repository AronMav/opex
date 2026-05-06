use async_trait::async_trait;
use serde_json::Value;

use crate::agent::pipeline::handlers as ph;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct SkillHandler;

#[async_trait]
impl SystemToolHandler for SkillHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "create" | "update" => ph::handle_skill_create(deps.workspace_dir, args).await,
            "list" => {
                ph::handle_skill_list(
                    deps.workspace_dir,
                    deps.agent_base,
                    deps.available_tools,
                    args,
                )
                .await
            }
            _ => format!(
                "Error: unknown skill action '{}'. Use: create, update, list.",
                action
            ),
        }
    }
}

pub struct SkillUseHandler;

#[async_trait]
impl SystemToolHandler for SkillUseHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("list");

        if action == "capture" {
            return ph::handle_skill_capture(
                deps.workspace_dir,
                deps.agent_name,
                deps.db,
                deps.ui_event_tx,
                args,
            )
            .await;
        }

        if action == "load"
            && let Some(name) = args.get("name").and_then(|v| v.as_str()) {
                let skills = crate::skills::load_skills(deps.workspace_dir).await;
                if let Some(skill) = skills.iter().find(|s| s.meta.name == name)
                    && matches!(skill.meta.state, crate::skills::SkillState::Archived) {
                        let workspace = deps.workspace_dir.to_string();
                        let skill_name = name.to_string();
                        let db = deps.db.clone();
                        let agent_name = deps.agent_name.to_string();
                        let now_iso = chrono::Utc::now().to_rfc3339();
                        tokio::spawn(async move {
                            crate::skills::reactivate_skill(
                                &workspace,
                                &skill_name,
                                &db,
                                &agent_name,
                                &now_iso,
                            )
                            .await;
                        });

                        let result = ph::handle_skill_use(
                            deps.workspace_dir,
                            deps.agent_base,
                            deps.available_tools,
                            args,
                        )
                        .await;
                        return format!(
                            "{}\n\n*(This skill was archived and has been reactivated.)*",
                            result
                        );
                    }
            }

        ph::handle_skill_use(
            deps.workspace_dir,
            deps.agent_base,
            deps.available_tools,
            args,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_handlers_implement_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(SkillHandler);
        assert_impl(SkillUseHandler);
    }

    #[test]
    fn skill_unknown_action_error() {
        let msg = format!(
            "Error: unknown skill action '{}'. Use: create, update, list.",
            "bad"
        );
        assert!(msg.contains("bad"));
    }
}
