mod workspace;
mod memory;
mod skills;
mod agent_tool;
mod tools_mgmt;
mod web;
mod code;
mod comms;
mod secrets_tool;
mod session;
mod tool_use;
mod todo;
mod file_scenario;
pub(crate) mod clarify;

use workspace::*;
use memory::*;
use skills::*;
use agent_tool::*;
use tools_mgmt::*;
use web::*;
use code::*;
use comms::*;
use secrets_tool::*;
use session::*;
use tool_use::*;
use todo::*;
use file_scenario::*;
use clarify::ClarifyHandler;

use crate::agent::tool_registry::SystemToolRegistry;

impl SystemToolRegistry {
    pub fn build() -> Self {
        let mut r = Self::new();
        r.register("workspace_write",  WorkspaceWriteHandler);
        r.register("workspace_read",   WorkspaceReadHandler);
        r.register("workspace_list",   WorkspaceListHandler);
        r.register("workspace_edit",   WorkspaceEditHandler);
        r.register("workspace_delete", WorkspaceDeleteHandler);
        r.register("workspace_rename", WorkspaceRenameHandler);
        r.register("apply_patch",      ApplyPatchHandler);
        r.register("memory",           MemoryToolHandler);
        r.register("message",          MessageHandler);
        r.register("cron",             CronHandler);
        r.register("agent",            AgentToolHandler);
        r.register("web_fetch",        WebFetchHandler);
        r.register("tool_create",      ToolCreateHandler);
        r.register("tool_list",        ToolListHandler);
        r.register("tool_test",        ToolTestHandler);
        r.register("tool_verify",      ToolVerifyHandler);
        r.register("tool_disable",     ToolDisableHandler);
        r.register("skill",            SkillHandler);
        r.register("skill_use",        SkillUseHandler);
        r.register("tool_discover",    ToolDiscoverHandler);
        r.register("secret_set",       SecretSetHandler);
        r.register("session",          SessionHandler);
        r.register("agents_list",      AgentsListHandler);
        r.register("browser_action",   BrowserActionHandler);
        r.register("todo",             TodoHandler);
        r.register("code_exec",        CodeExecHandler);
        r.register("git",              GitToolHandler);
        r.register("canvas",           CanvasHandler);
        r.register("rich_card",        RichCardHandler);
        r.register("process",          ProcessHandler);
        r.register("tool_use",         ToolUseHandler);
        r.register("file_scenario",    FileScenarioHandler);
        r.register("clarify",          ClarifyHandler);
        r
    }
}

#[cfg(test)]
mod registry_tests {
    use super::SystemToolRegistry;

    #[test]
    fn file_scenario_handler_is_registered() {
        let r = SystemToolRegistry::build();
        // dispatch returns None only when the name is unknown; a known handler
        // returns Some(_). We assert registration without invoking DB work by
        // checking the handler map via a known-unknown contrast.
        assert!(
            r.is_registered("file_scenario"),
            "file_scenario must be registered in SystemToolRegistry::build()"
        );
        assert!(
            !r.is_registered("definitely_not_a_tool"),
            "control: unknown tool must not be registered"
        );
    }
}
