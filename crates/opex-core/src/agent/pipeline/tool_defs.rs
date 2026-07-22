//! Pipeline step: tool_defs — tool definitions assembly (migrated from engine_tool_defs.rs).
//!
//! Pure functions that build `Vec<ToolDefinition>` from agent config + capability flags.
//! No `&self` / no `AgentEngine` dependency — enables testing and reuse outside the engine.

use opex_types::ToolDefinition;

use crate::config::ToolGroups;

// ── Static catalogue ────────────────────────────────────────────────────

/// All system (internal) tool names — single source of truth.
///
/// Derived at runtime from `build_internal_tool_definitions()` called with
/// maximal context. Cached once via `OnceLock` so subsequent calls are O(1).
///
/// Used by the API to populate tool policy UI without needing an engine instance.
///
/// Related: [`crate::agent::pipeline::dispatch::SYSTEM_TOOL_NAMES`] is a
/// **different** list — only tools `filter_tools_by_policy` admits unconditionally
/// (after the deny check). It is hand-maintained because the "policy passes
/// through" semantic does not derive from `build_internal_tool_definitions`
/// with any context. For "is X a known tool?" questions always use this function.
pub fn all_system_tool_names() -> &'static [&'static str] {
    static ALL_NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    ALL_NAMES.get_or_init(|| {
        static MAX_GROUPS: ToolGroups = ToolGroups {
            git: true,
            tool_management: true,
            skill_editing: true,
            session_tools: true,
        };
        let ctx = ToolDefsContext {
            is_base: true,
            groups: &MAX_GROUPS,
            default_timezone: "UTC",
            has_sandbox: true,
            browser_renderer_url: "",
            lsp_enabled: true,
        };
        build_internal_tool_definitions(&ctx)
            .into_iter()
            .map(|d| Box::leak(d.name.into_boxed_str()) as &'static str)
            .collect()
    })
    .as_slice()
}

/// Names of tools always preloaded in the LLM tools array when the
/// dispatcher is enabled. Source of truth for `engine/context_builder.rs`
/// partition logic. Spec: tool-dispatcher-design.md §"Core / extension partition".
pub fn static_core_tool_names() -> &'static [&'static str] {
    &[
        "workspace_read",
        "workspace_write",
        "workspace_edit",
        "workspace_list",
        "code_exec",
        "memory",
        "agent",
        "skill_use",
        "web_fetch",
        "tool_use",
    ]
}

// ── Context for building tool definitions ───────────────────────────────

/// Read-only inputs required to assemble the tool definition list.
pub struct ToolDefsContext<'a> {
    pub is_base: bool,
    pub groups: &'a ToolGroups,
    pub default_timezone: &'a str,
    pub has_sandbox: bool,
    pub browser_renderer_url: &'a str,
    /// Whether the `lsp` tool is emitted (gated on `[lsp] enabled`, like
    /// `browser_action` is gated on browser-renderer availability). The
    /// authoritative catalogue `all_system_tool_names()` forces this `true`.
    pub lsp_enabled: bool,
}

// ── Helper: resolve groups with default fallback ────────────────────────

/// Resolve tool group settings (from agent config or defaults).
pub fn resolve_tool_groups(tools: Option<&crate::config::AgentToolPolicy>) -> &ToolGroups {
    static DEFAULT: ToolGroups = ToolGroups {
        git: true,
        tool_management: true,
        skill_editing: true,
        session_tools: true,
    };
    tools.map(|t| &t.groups).unwrap_or(&DEFAULT)
}

// ── Main builder ────────────────────────────────────────────────────────

/// Build the full list of internal (system) tool definitions.
///
/// This is a pure function — it reads only the provided context to decide which tools
/// to include and how to describe them.
pub fn build_internal_tool_definitions(ctx: &ToolDefsContext<'_>) -> Vec<ToolDefinition> {
    let groups = ctx.groups;
    let mut tools = vec![
        ToolDefinition {
            name: "workspace_write".to_string(),
            description: "Create or overwrite a file in your workspace. Bare names go to your agent dir; paths with '/' are relative to workspace root.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "File path. Bare name → your agent dir. With '/' → relative to workspace root. E.g.: 'SOUL.md', 'notes/todo.md', 'USER.md'"
                    },
                    "content": {
                        "type": "string",
                        "description": "New content for the file (replaces entire file)"
                    }
                },
                "required": ["filename", "content"]
            }),
        },
        ToolDefinition {
            name: "workspace_read".to_string(),
            description: "Read a file from the workspace. Returns full text content.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "File path relative to workspace (e.g. 'SOUL.md', 'notes/todo.md')"
                    }
                },
                "required": ["filename"]
            }),
        },
        ToolDefinition {
            name: "workspace_list".to_string(),
            description: "List files in a workspace directory. Returns names only.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "directory": {
                        "type": "string",
                        "description": "Subdirectory to list (default: root of workspace)",
                        "default": "."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "workspace_edit".to_string(),
            description: "Apply targeted text edits to a workspace file. Specify search/replace pairs.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "File path relative to workspace"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Text to find in the file (must be an exact match)"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Text to replace it with"
                    }
                },
                "required": ["filename", "old_text", "new_text"]
            }),
        },
        ToolDefinition {
            name: "apply_patch".to_string(),
            description: "Apply a multi-file patch in the V4A format (context-based, no line numbers). Best for several edits across one or more files at once. Envelope:\n*** Begin Patch\n*** Update File: notes/todo.md\n@@ optional anchor\n context line\n-removed line\n+added line\n*** Add File: notes/new.md\n+new file line\n*** End Patch\nUpdate hunks: ' ' context, '-' remove, '+' add; the context must match the current file. Add File: '+' lines are the new file's content. Atomic — if any hunk fails to match, nothing is written. Supports Update + Add only (use workspace_delete/workspace_rename for those).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": "The full V4A patch envelope from '*** Begin Patch' to '*** End Patch'."
                    }
                },
                "required": ["patch"]
            }),
        },
        ToolDefinition {
            name: "workspace_delete".to_string(),
            description: "Delete a file or directory from your workspace. Core identity files (SOUL.md, IDENTITY.md, MEMORY.md, HEARTBEAT.md) are protected and cannot be deleted.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "File or directory path relative to workspace (e.g. 'notes/old.md')"
                    }
                },
                "required": ["filename"]
            }),
        },
        ToolDefinition {
            name: "workspace_rename".to_string(),
            description: "Move or rename a file/directory in your workspace. Works within and across workspace subdirectories (e.g. rename a provider file, move a file to a subfolder).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "old_path": {
                        "type": "string",
                        "description": "Current path relative to workspace (e.g. 'notes/old_name.md')"
                    },
                    "new_path": {
                        "type": "string",
                        "description": "New path relative to workspace (e.g. 'notes/new_name.md')"
                    }
                },
                "required": ["old_path", "new_path"]
            }),
        },
        ToolDefinition {
            name: "agent".to_string(),
            description: "Delegate to or query other agents: action=ask/status/kill. See multi-agent-coordination skill for patterns.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["ask", "status", "kill"],
                        "description": "Action to perform"
                    },
                    "target": {
                        "type": "string",
                        "description": "Agent name (required for ask & kill, optional for status)"
                    },
                    "text": {
                        "type": "string",
                        "description": "Message / task text (required for ask)"
                    },
                    "fresh": {
                        "type": "boolean",
                        "description": "For ask: when true, kill any existing instance of target first and start fresh. Default: false (continue existing dialog if peer is alive).",
                        "default": false
                    }
                },
                "required": ["action"]
            }),
        },
        ToolDefinition {
            name: "web_fetch".to_string(),
            description: "Fetch URL content. Returns rendered text (HTML stripped). Cached for 5 min.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch (http or https)"
                    },
                    "max_length": {
                        "type": "integer",
                        "description": "Maximum response length in characters (default: 50000)",
                        "default": 50000
                    }
                },
                "required": ["url"]
            }),
        },
        ToolDefinition {
            name: "file_handler".to_string(),
            description: "Run a file/URL handler (e.g. summarize a video, transcribe audio, extract a document, save a file to workspace). When the user sends a video/file link or an uploaded file, the system lists the matching handlers in context; present those options to the user and, once they pick one, call this with action=\"run\". Use action=\"list\" to re-fetch the options. For action=\"list\" with a session, an interactive clickable menu is automatically shown to the user — reply with an EMPTY message (no text). The result (e.g. the video summary) is delivered to the chat asynchronously when it finishes.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "run"],
                        "description": "\"list\" the available handlers for the source, or \"run\" the chosen one."
                    },
                    "source_url": {
                        "type": "string",
                        "description": "The video/file URL the user sent (mutually exclusive with upload_id)."
                    },
                    "upload_id": {
                        "type": "string",
                        "description": "The uploaded file's id (mutually exclusive with source_url)."
                    },
                    "handler_id": {
                        "type": "string",
                        "description": "Required for action=run — the handler the user chose (must be one from action=list)."
                    },
                    "args": {
                        "type": "object",
                        "description": "Optional handler-specific parameters. Examples: {\"path\": \"uploads/file.json\"} for save, {\"prompt\": \"Describe in Russian\"} for describe, {\"max_chars\": 5000} for extract_document.",
                        "properties": {
                            "path": { "type": "string", "description": "For save: relative path in workspace (e.g. 'uploads/file.json')" },
                            "prompt": { "type": "string", "description": "For describe: custom prompt for vision model" },
                            "max_chars": { "type": "integer", "description": "For extract_document: max characters to extract" }
                        }
                    }
                },
                "required": ["action"]
            }),
        },
        ToolDefinition {
            name: "memory".to_string(),
            description: "Long-term memory: action=search/index/reindex/get/delete/update. Use pinned=true for permanent facts.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["search", "index", "reindex", "get", "delete", "update"],
                        "description": "Memory action to perform"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query (for search)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to remember (for index) or fact text (for update)"
                    },
                    "source": {
                        "type": "string",
                        "description": "Source label for the memory entry (for index/get)",
                        "default": "manual"
                    },
                    "pinned": {
                        "type": "boolean",
                        "description": "Pin as permanent memory, no decay (for index)",
                        "default": false
                    },
                    "shared": {
                        "type": "boolean",
                        "description": "Make visible to all agents (for index). Default: private to this agent only.",
                        "default": false
                    },
                    "clear_existing": {
                        "type": "boolean",
                        "description": "Delete all existing chunks before re-indexing (for reindex)",
                        "default": false
                    },
                    "include_sessions": {
                        "type": "boolean",
                        "description": "Also index session transcripts into memory (for reindex, default: true)",
                        "default": true
                    },
                    "graph": {
                        "type": "boolean",
                        "description": "Run GraphRAG entity extraction (for reindex, default: true)",
                        "default": true
                    },
                    "chunk_id": {
                        "type": "string",
                        "description": "UUID of a memory chunk (for get/delete)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (for search/get)",
                        "default": 10
                    },
                    "section": {
                        "type": "string",
                        "description": "Section heading in MEMORY.md (for update, e.g. 'User', 'Projects')"
                    },
                    "sub_action": {
                        "type": "string",
                        "enum": ["add", "update", "remove"],
                        "description": "MEMORY.md edit action (for update): add bullet, update existing, remove bullet"
                    }
                },
                "required": ["action"]
            }),
        },
        ToolDefinition {
            name: "message".to_string(),
            description: "Perform actions on the current chat message: react with emoji, pin/unpin, edit text, delete, or reply. Message context (chat, message ID) is provided automatically.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "Action to perform",
                        "enum": ["react", "pin", "unpin", "edit", "delete", "reply"]
                    },
                    "text": {
                        "type": "string",
                        "description": "New text (for edit/reply actions)"
                    },
                    "emoji": {
                        "type": "string",
                        "description": "Emoji for reaction (e.g. '👍', '❤️', '🔥')"
                    }
                },
                "required": ["action"]
            }),
        },
    ];

    // cron: base agents get full CRUD, regular agents get read-only
    tools.push(if ctx.is_base {
        ToolDefinition {
            name: "cron".to_string(),
            description: "Manage scheduled tasks (cron jobs). RULES: 1) ALWAYS list first. 2) To modify an existing job, use 'update' with job_id. NEVER use remove+add. 3) When using 'add', set 'agent' to the target agent name.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "history", "add", "update", "remove", "run"] },
                    "name": { "type": "string", "description": "Job name (for add/update)" },
                    "cron": { "type": "string", "description": "Cron expression: min hour dom mon dow" },
                    "timezone": { "type": "string", "description": format!("Timezone (default: {})", ctx.default_timezone) },
                    "task": { "type": "string", "description": "Task message for the agent (for add/update/run)" },
                    "job_id": { "type": "string", "description": "Job UUID (for remove/history/run)" },
                    "limit": { "type": "integer", "description": "Max results (default 10)" },
                    "announce_to": { "type": "object", "description": "Delivery target: {\"channel\": \"telegram\", \"chat_id\": 123}" },
                    "autonomous_goal": { "type": "string", "description": "Optional. If set, this (recurring) job runs as a durable goal-driven session — re-driven to completion after a crash — instead of a one-shot task." },
                    "agent": { "type": "string", "description": "Target agent name (default: self)" }
                },
                "required": ["action"]
            }),
        }
    } else {
        ToolDefinition {
            name: "cron".to_string(),
            description: "View your scheduled tasks. To create or modify jobs, delegate to the base agent.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "history", "runs"] },
                    "job_id": { "type": "string", "description": "Job UUID (for history)" },
                    "limit": { "type": "integer", "description": "Max results (default 10)" }
                },
                "required": ["action"]
            }),
        }
    });

    // ── Tool management (optional group) ────────────────────────────────
    if groups.tool_management {
        tools.extend(vec![
        ToolDefinition {
            name: "tool_create".to_string(),
            description: "Create a new typed HTTP tool from a YAML definition. The tool is placed in draft status and must be tested (tool_test) and verified (tool_verify) before use. Use when the user wants to connect a new API.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tool name (snake_case, lowercase, e.g. wms_get_stock)"
                    },
                    "description": {
                        "type": "string",
                        "description": "Human-readable description of what this tool does"
                    },
                    "endpoint": {
                        "type": "string",
                        "description": "HTTP endpoint URL. Use {param} for path parameters."
                    },
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                        "description": "HTTP method"
                    },
                    "parameters": {
                        "type": "object",
                        "description": "Map of parameter name → definition. Each has: type, required, location (path/query/body/header), description."
                    },
                    "auth": {
                        "type": "object",
                        "description": "Auth config: {type: bearer_env, key: ENV_VAR} or {type: api_key_header, header_name: X-API-Key, key: ENV_VAR}"
                    },
                    "headers": {
                        "type": "object",
                        "description": "Static HTTP headers to include"
                    },
                    "body_template": {
                        "type": "string",
                        "description": "Optional JSON body template with {{param}} substitution for non-standard body structures"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional tags for categorization"
                    }
                },
                "required": ["name", "description", "endpoint", "method"]
            }),
        },
        ToolDefinition {
            name: "tool_list".to_string(),
            description: "List registered YAML tools by status. Shows name, description, endpoint, and status for each tool.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["all", "verified", "draft", "disabled"],
                        "description": "Filter by status (default: all)",
                        "default": "all"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "tool_test".to_string(),
            description: "Test a tool (including draft tools) with specific parameters. Shows the HTTP request that would be made and the actual response. Use to verify a tool works before calling tool_verify.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "description": "Name of the tool to test"
                    },
                    "params": {
                        "type": "object",
                        "description": "Parameters to pass to the tool"
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true, show the HTTP request without executing it",
                        "default": false
                    }
                },
                "required": ["tool_name"]
            }),
        },
        ToolDefinition {
            name: "tool_verify".to_string(),
            description: "Promote a draft tool to verified status, making it available in LLM context. Only call after testing with tool_test.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "description": "Name of the draft tool to verify"
                    }
                },
                "required": ["tool_name"]
            }),
        },
        ToolDefinition {
            name: "tool_disable".to_string(),
            description: "Disable a tool by moving it to disabled status. The tool file is preserved but the tool is excluded from LLM context and cannot be called.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "description": "Name of the tool to disable"
                    }
                },
                "required": ["tool_name"]
            }),
        },
        ]);
    }

    // secret_set: base agents get global option, regular agents only scoped
    {
        let mut props = serde_json::json!({
            "name": {
                "type": "string",
                "description": "Secret name (uppercase, e.g. API_NINJAS_KEY, BRAVE_SEARCH_API_KEY)"
            },
            "value": {
                "type": "string",
                "description": "Secret value"
            },
            "description": {
                "type": "string",
                "description": "Optional description of the secret"
            }
        });
        let desc = if ctx.is_base {
            props.as_object_mut().expect("props is always an object (constructed inline)").insert("global".to_string(), serde_json::json!({
                "type": "boolean",
                "description": "If true, store as global (available to all agents). Default: false (scoped to current agent)."
            }));
            "Store an API key or secret in the encrypted vault. Available as env var for YAML tools (auth.key). Set global=true for all agents. NEVER repeat the secret value in your response."
        } else {
            "Store an API key or secret in the encrypted vault, scoped to this agent. Available as env var for YAML tools (auth.key). NEVER repeat the secret value in your response."
        };
        tools.push(ToolDefinition {
            name: "secret_set".to_string(),
            description: desc.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": props,
                "required": ["name", "value"]
            }),
        });
    }
    tools.extend(vec![
        ToolDefinition {
            name: "canvas".to_string(),
            description: concat!(
                "Display rich visual content in the dedicated Canvas panel of the UI. ",
                "Use this when the user asks to show, visualize, draw, render, or display something visually.\n\n",
                "For content_type='html': write a complete self-contained HTML page with inline CSS/JS. ",
                "The HTML is rendered in a sandboxed iframe.\n\n",
                "STRICT DESIGN RULES (violations will be rejected):\n",
                "- NEVER use emoji (🌤️☁️🌡️💧💨🚀📊✨ etc.) as icons — draw SVG icons or use CSS shapes instead\n",
                "- NEVER use purple/indigo/violet gradients — choose warm earth tones, teals, ambers, or monochrome\n",
                "- NEVER make 3 equal cards in a row — use asymmetric layouts with varied sizes\n",
                "- NEVER center everything — use left-aligned text, asymmetric grids, varied whitespace\n",
                "- Use distinctive fonts: vary weights dramatically (200 vs 800), use serif+sans-serif mix\n",
                "- Add depth: layered shadows, subtle borders, noise textures, glassmorphism\n",
                "- Add life: CSS transitions on hover, staggered @keyframe fade-ins, subtle transforms\n",
                "- Dark themes: use rich deep colors (#1a1a2e, #0a192f, #2d1b33) not flat black\n",
                "- The design must look like a human designer crafted it, NOT like generic AI output\n\n",
                "Actions: present (show content), push_data (JSON table), clear, run_js (execute JS), snapshot (screenshot).\n",
                "Max content size: 5MB. Always include a text summary in your chat message — the user may not see the canvas."
            ).to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["present", "push_data", "clear", "run_js", "snapshot"],
                        "description": "The canvas action to perform."
                    },
                    "content_type": {
                        "type": "string",
                        "enum": ["markdown", "html", "url", "json"],
                        "description": "Content format for 'present' action. Use 'html' for rich visual content (dashboards, charts, styled reports). Use 'markdown' for text-heavy content. Default: markdown."
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to display. For html: a complete HTML document with inline styles. For markdown: markdown text. For url: a URL to embed. For json: a JSON string."
                    },
                    "title": {
                        "type": "string",
                        "description": "Title shown in the canvas panel header."
                    },
                    "code": {
                        "type": "string",
                        "description": "JavaScript code to execute in the canvas (for run_js action only)."
                    }
                },
                "required": ["action"]
            }),
        },
        ToolDefinition {
            name: "rich_card".to_string(),
            description: "Display a rich card inline in the chat message. Use for tables, metrics, and structured data that should appear directly in the conversation flow (not in the separate canvas panel).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "card_type": {
                        "type": "string",
                        "enum": ["table", "metric"],
                        "description": "Card type: table (columns+rows), metric (number with label and trend)."
                    },
                    "title": {
                        "type": "string",
                        "description": "Card title."
                    },
                    "columns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Column headers (for table card_type)."
                    },
                    "rows": {
                        "type": "array",
                        "items": { "type": "array", "items": {} },
                        "description": "Table rows, each row is an array of cell values (for table card_type)."
                    },
                    "value": {
                        "type": "string",
                        "description": "Metric value (for metric card_type)."
                    },
                    "label": {
                        "type": "string",
                        "description": "Metric label (for metric card_type)."
                    },
                    "trend": {
                        "type": "string",
                        "enum": ["up", "down", "flat"],
                        "description": "Trend direction (for metric card_type)."
                    }
                },
                "required": ["card_type"]
            }),
        },
    ]);

    // ── Skill editing (optional group) ──────────────────────────────────
    if groups.skill_editing {
        tools.push(ToolDefinition {
            name: "skill".to_string(),
            description: "Manage skill scenarios. Actions: create (new skill .md with YAML frontmatter), update (overwrite existing), list (show all). Skills are auto-matched by trigger keywords and inject instructions into LLM context.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "update", "list"],
                        "description": "Skill action to perform"
                    },
                    "name": {
                        "type": "string",
                        "description": "Skill identifier (snake_case, e.g. research_task) — for create/update"
                    },
                    "description": {
                        "type": "string",
                        "description": "Short description of what this skill does"
                    },
                    "triggers": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Keywords/phrases that activate this skill (Russian or English)"
                    },
                    "tools_required": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Tool names restricted to when this skill is active (empty = all tools)"
                    },
                    "instructions": {
                        "type": "string",
                        "description": "Step-by-step instructions injected into system prompt when skill is active (Markdown)"
                    },
                    "priority": {
                        "type": "integer",
                        "description": "Priority when multiple skills match (higher wins, default: 0)",
                        "default": 0
                    }
                },
                "required": ["action"]
            }),
        });
    }

    // skill_use: on-demand skill loading (always available, not gated by skill_editing)
    tools.push(ToolDefinition {
        name: "skill_use".to_string(),
        description: "Discover and load reusable skills. action=list to enumerate, action=load to read.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "load", "capture"],
                    "description": "list = show catalog, load = get full skill instructions, capture = create a new skill"
                },
                "name": {
                    "type": "string",
                    "description": "Skill name (for load or capture). kebab-case, e.g. 'image-resize-workflow'"
                },
                "description": {
                    "type": "string",
                    "description": "One-sentence summary of what this skill teaches (for capture)"
                },
                "triggers": {
                    "type": "string",
                    "description": "Comma-separated phrases that should activate this skill (for capture, optional)"
                },
                "tools_required": {
                    "type": "string",
                    "description": "Comma-separated tool names this skill needs (for capture, optional)"
                },
                "instructions": {
                    "type": "string",
                    "description": "Full skill body in markdown (for capture)"
                }
            },
            "required": ["action"]
        }),
    });

    // tool_discover is part of tool_management group
    if groups.tool_management {
        tools.extend(vec![
        ToolDefinition {
            name: "tool_discover".to_string(),
            description: "Auto-generate draft HTTP tools from an OpenAPI 2.x/3.x spec URL. Fetches the spec, parses all API operations, and creates a draft YAML file for each. Use tool_test + tool_verify to activate the discovered tools.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "spec_url": {
                        "type": "string",
                        "description": "URL of the OpenAPI/Swagger JSON or YAML spec (e.g. https://api.example.com/openapi.json)"
                    },
                    "prefix": {
                        "type": "string",
                        "description": "Optional name prefix for all generated tools (e.g. 'myapi' → 'myapi_get_users'). Use to avoid naming conflicts."
                    }
                },
                "required": ["spec_url"]
            }),
        },
        ]);
    }

    // ── Git tools (optional group) ──────────────────────────────────────
    if groups.git {
        tools.push(ToolDefinition {
            name: "git".to_string(),
            description: "Git operations on workspace repositories. Actions: status, diff, log, commit, add, push, pull, branch, clone.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "diff", "log", "commit", "add", "push", "pull", "branch", "clone"],
                        "description": "Git action to perform"
                    },
                    "directory": {
                        "type": "string",
                        "description": "Subdirectory in workspace containing the git repo. Default: workspace root."
                    },
                    "message": { "type": "string", "description": "Commit message (for commit)" },
                    "files": { "type": "array", "items": {"type": "string"}, "description": "Files to stage (for add). Use [\".\"] for all." },
                    "limit": { "type": "integer", "description": "Number of commits (for log, default 20)" },
                    "oneline": { "type": "boolean", "description": "Compact format (for log, default true)" },
                    "url": { "type": "string", "description": "Repository URL (for clone)" },
                    "branch_action": { "type": "string", "enum": ["list", "create", "switch", "delete"], "description": "Branch sub-action (for branch, default list)" },
                    "name": { "type": "string", "description": "Branch name (for branch create/switch/delete)" }
                },
                "required": ["action"]
            }),
        });
    }

    // ── Session tools (optional group) ──────────────────────────────────
    if groups.session_tools {
        tools.push(ToolDefinition {
            name: "session".to_string(),
            description: "Manage conversation sessions. Actions: list (recent sessions), history (messages from session), search (find messages by content), context (current session metadata), send (message to user/chat), export (full session as text/json).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "history", "search", "context", "send", "export"],
                        "description": "Session action to perform"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session UUID (for history/export)"
                    },
                    "query": {
                        "type": "string",
                        "description": "Text to search for (for search)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Message text to send (for send)"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "Target user/chat ID (for send)"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Filter by or target channel (for list/send, default: telegram)"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["text", "json"],
                        "description": "Output format (for export, default: text)",
                        "default": "text"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (for list/history/search)",
                        "default": 20
                    }
                },
                "required": ["action"]
            }),
        });
    }

    // clarify: mid-run user clarification (sequential only — NOT in static_core_tool_names)
    tools.push(ToolDefinition {
        name: "clarify".to_string(),
        description: "Ask the user a clarifying question and wait for a response before continuing. \
Use when you need a decision or missing information that only the user can provide. \
Do NOT list options in the question field — put them in choices instead. \
Only available in interactive contexts (web UI or Telegram); returns an error in cron/inter-agent contexts.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user. Do not enumerate options here — use choices for that."
                },
                "choices": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of up to 4 answer choices presented as buttons. Omit for open-ended text input.",
                    "maxItems": 4
                }
            },
            "required": ["question"]
        }),
    });

    // agents_list is always available (core tool)
    tools.push(ToolDefinition {
        name: "agents_list".to_string(),
        description: "List all agents in the system with their status, provider, and model.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {}
        }),
    });

    // profile: view and switch the agent's configured providers/models
    tools.push(ToolDefinition {
        name: "profile".to_string(),
        description: "View your profile slots (text, vision, tts, stt, imagegen, websearch) with their provider/model list, or switch to a different provider/model for the current turn. action=\"show\" to list all slots; action=\"switch\" with slot+provider+optional model to change for this turn only.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["show", "switch"],
                    "description": "\"show\" = list all slots with their providers/models; \"switch\" = change the active provider/model for this turn"
                },
                "slot": {
                    "type": "string",
                    "description": "Capability slot to switch (for switch): text, vision, tts, stt, imagegen, websearch, compaction"
                },
                "provider": {
                    "type": "string",
                    "description": "Provider name to switch to (for switch). Must be one of the providers listed in the slot."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override (for switch). If omitted, uses the provider's default_model."
                }
            },
            "required": ["action"]
        }),
    });

    tools.push(ToolDefinition {
        name: "tool_use".to_string(),
        description: "Discover and invoke extension tools (YAML, MCP, rare system tools). Actions: \"search\" (find tools by query), \"describe\" (load full schema for a tool by name), \"call\" (invoke a tool by name with arguments). For frequent tools (workspace_*, code_exec, memory, agent, skill_use, web_fetch) call them directly — do NOT use tool_use for those.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search", "describe", "call"],
                    "description": "Operation to perform"
                },
                "query": { "type": "string", "description": "For action=search" },
                "name":  { "type": "string", "description": "Tool name (describe/call)" },
                "arguments": { "type": "object", "description": "For action=call" }
            },
            "required": ["action"]
        }),
    });

    tools.push(ToolDefinition {
        name: "todo".to_string(),
        description: "Maintain a structured task list for THIS session. It persists across turns and survives context compression. Use mode=read to see the list and mode=write to upsert items. Plan multi-step work here and update statuses as you go.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["read", "write"], "description": "read = return current list; write = upsert items" },
                "strategy": { "type": "string", "enum": ["merge", "replace"], "description": "write only: merge (upsert by id, default) or replace (overwrite whole list)" },
                "items": {
                    "type": "array",
                    "description": "write only: the tasks",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "stable task identifier" },
                            "content": { "type": "string", "description": "task description (max 4000 chars)" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "done", "cancelled"] }
                        },
                        "required": ["id", "content", "status"]
                    }
                }
            },
            "required": ["mode"]
        }),
    });

    // LSP tooling (conditional on `[lsp] enabled`; execution separately gated
    // by the LspManager in AgentDeps). `all_system_tool_names()` forces this on.
    if ctx.lsp_enabled {
        tools.push(ToolDefinition {
            name: "lsp".to_string(),
            description: "IDE intelligence for the agent's Python project files via an in-process language-server pool (Pyright).\n\nActions:\n• diagnostics — report type errors and warnings in `file` (no cursor needed)\n• definition  — go to definition of the symbol at `file:line:character`\n• references  — find all references to the symbol at `file:line:character`\n• hover       — show type/doc hover for the symbol at `file:line:character`\n• symbols     — list all top-level symbols (classes, functions, variables) in `file`\n• rename      — rename the symbol at `file:line:character` to `new_name`; applies the resulting WorkspaceEdit to all affected files atomically and returns a summary\n\nPositions are 0-based (LSP convention). `file` is a workspace-relative path (e.g. `myproject/app.py`).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["diagnostics", "definition", "references", "hover", "symbols", "rename"],
                        "description": "LSP operation to perform."
                    },
                    "file": {
                        "type": "string",
                        "description": "Workspace-relative path to the source file (e.g. 'myproject/main.py')."
                    },
                    "line": {
                        "type": "integer",
                        "description": "0-based line number. Required for definition, references, hover, rename."
                    },
                    "character": {
                        "type": "integer",
                        "description": "0-based byte offset within the line (utf-8). Required for definition, references, hover, rename."
                    },
                    "new_name": {
                        "type": "string",
                        "description": "Replacement identifier. Required for rename."
                    }
                },
                "required": ["action", "file"]
            }),
        });
    }

    // Browser automation (conditional on browser-renderer availability)
    if ctx.browser_renderer_url != "disabled" {
        tools.push(ToolDefinition {
            name: "browser_action".to_string(),
            description: "Interact with web pages via headless browser. Workflow: create_session → navigate → actions (click/type/scroll/hover/drag/back/press/screenshot/etc.) → close. scroll is required for dynamic pages (infinite scroll, dropdowns). JS dialogs are auto-accepted by default; use set_dialog to change. Sessions auto-expire after 5 min idle.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create_session", "navigate", "click", "type", "fill", "screenshot", "wait", "text", "evaluate", "content", "scroll", "hover", "drag", "back", "press", "set_dialog", "close"],
                        "description": "Action to perform. Start with create_session, end with close. scroll/hover/drag/back/press operate on the current page."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session ID from create_session. Required for all actions except create_session."
                    },
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to (for navigate action)."
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for click/type/wait/text actions."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type (for type action)."
                    },
                    "js": {
                        "type": "string",
                        "description": "JavaScript expression to evaluate (for evaluate action)."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (1-60, default 10)."
                    },
                    "full_page": {
                        "type": "boolean",
                        "description": "Full page screenshot (for screenshot action, default false)."
                    },
                    "fields": {
                        "type": "object",
                        "description": "Map of selector→value for fill action (bulk form fill)."
                    },
                    "key": { "type": "string", "description": "Keyboard key for press action (e.g. 'Enter', 'Escape', 'Tab')." },
                    "dx": { "type": "integer", "description": "Horizontal scroll delta in pixels (scroll action)." },
                    "dy": { "type": "integer", "description": "Vertical scroll delta in pixels (scroll action). Positive = down." },
                    "to": { "type": "string", "enum": ["top", "bottom"], "description": "Scroll target shortcut (scroll action)." },
                    "to_selector": { "type": "string", "description": "Target CSS selector for drag action." },
                    "accept": { "type": "boolean", "description": "set_dialog: accept (true) or dismiss (false) future JS dialogs." },
                    "prompt_text": { "type": "string", "description": "set_dialog: text to enter for window.prompt() dialogs." }
                },
                "required": ["action"]
            }),
        });
    }

    // code_exec: for base agents runs on host; for others runs in Docker sandbox
    if ctx.is_base && !ctx.has_sandbox {
    tools.push(ToolDefinition {
        name: "code_exec".to_string(),
        description: "Execute bash or Python on the host. Full filesystem access; cwd = binary dir. Returns stdout/stderr. The 'code' parameter is REQUIRED — pass the actual code or commands as a string.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "REQUIRED. The code or commands to execute as a string. For bash: shell commands (e.g. 'ls -la /home'). For Python: full script text."
                },
                "language": {
                    "type": "string",
                    "description": "Programming language: 'bash' (default for host operations) or 'python'",
                    "enum": ["bash", "python"],
                    "default": "bash"
                },
                "packages": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Python packages to install before execution. Only for Python."
                }
            },
            "required": ["code"]
        }),
    });
    } else if ctx.has_sandbox {
        tools.extend(build_sandbox_tool_definitions());
    }

    // code_orchestrate (codemode): tools-as-code for base agents with sandbox.
    // The LLM writes a Python script that calls tools programmatically via
    // the loopback /api/sandbox/tool-call endpoint. v1: base agents only.
    if ctx.is_base && ctx.has_sandbox {
        tools.push(ToolDefinition {
            name: "code_orchestrate".to_string(),
            description: "Run a Python script that calls tools programmatically (loops, parallel calls, aggregation in one turn). The script has access to a `tools` object with one method per available tool (e.g. tools.workspace_read(path='...')). Use tools_search(query) to discover tool signatures. Available to base agents only (v1).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "Python script to execute. Use `tools.tool_name(...)` to call tools, `tools_search(query)` to discover signatures. The script runs in the Docker sandbox with network access to core's loopback endpoint."
                    }
                },
                "required": ["code"]
            }),
        });
    }

    if ctx.is_base {
        tools.push(ToolDefinition {
            name: "process".to_string(),
            description: "Manage background processes (base only). Actions: start (run bash command in background), status (check running/done), logs (get output), kill (stop process).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "status", "logs", "kill"],
                        "description": "Process action to perform"
                    },
                    "command": {
                        "type": "string",
                        "description": "Bash command to run (for start)"
                    },
                    "working_directory": {
                        "type": "string",
                        "description": "Working directory (for start, default: OPEX binary dir)"
                    },
                    "process_id": {
                        "type": "string",
                        "description": "Process ID (for status/logs/kill)"
                    },
                    "tail_lines": {
                        "type": "integer",
                        "description": "Last N lines (for logs, default 50)"
                    }
                },
                "required": ["action"]
            }),
        });
    }

    tools
}

// ── Sandbox tool definition ─────────────────────────────────────────────

/// Returns the code_exec tool definition for sandbox mode.
pub fn build_sandbox_tool_definitions() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "code_exec".to_string(),
        description: "Execute bash or Python in an isolated container. Workspace files NOT accessible — pass data via variables. Returns stdout/stderr. The 'code' parameter is REQUIRED — pass the actual code as a string.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "REQUIRED. The code to execute as a string. For Python: full script text. For bash: shell commands."
                },
                "language": {
                    "type": "string",
                    "description": "Programming language: 'python' (default) or 'bash'",
                    "enum": ["python", "bash"],
                    "default": "python"
                },
                "packages": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Python packages to install before execution (e.g. ['pandas', 'numpy']). Only for Python."
                }
            },
            "required": ["code"]
        }),
    }]
}

// ── Subagent filtering ──────────────────────────────────────────────────

/// Filter tool definitions for subagent use: exclude denied tools, optionally filter by whitelist.
///
/// `denied_tools` is owned-strings (typically the output of
/// `subagent::runtime_subagent_denylist(&DelegationConfig)` — anchored to
/// `SUBAGENT_DENIED_TOOLS`; see audit 2026-05-08 groups T and FF for the rationale).
pub fn filter_for_subagent(
    all_tools: Vec<ToolDefinition>,
    denied_tools: &[String],
    allowed_tools: Option<&[String]>,
) -> Vec<ToolDefinition> {
    let safe: Vec<_> = all_tools
        .into_iter()
        .filter(|t| !denied_tools.contains(&t.name))
        .collect();
    match allowed_tools {
        Some(whitelist) => safe
            .into_iter()
            .filter(|t| whitelist.iter().any(|a| a == &t.name))
            .collect(),
        None => safe,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_in_system_tool_names() {
        let names = all_system_tool_names();
        assert!(names.contains(&"agent"), "agent must be in all_system_tool_names()");
    }

    #[test]
    fn build_tool_defs_base_agent() {
        let groups = ToolGroups {
            git: true,
            tool_management: true,
            skill_editing: true,
            session_tools: true,
        };
        let ctx = ToolDefsContext {
            is_base: true,
            groups: &groups,
            default_timezone: "UTC",
            has_sandbox: false,
            browser_renderer_url: "http://localhost:9020",
            lsp_enabled: false,
        };
        let tools = build_internal_tool_definitions(&ctx);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"workspace_write"));
        assert!(names.contains(&"code_exec"));
        assert!(names.contains(&"process"));
        assert!(names.contains(&"git"));
        assert!(names.contains(&"session"));
        assert!(names.contains(&"tool_create"));
    }

    #[test]
    fn build_tool_defs_regular_agent() {
        let groups = ToolGroups {
            git: false,
            tool_management: false,
            skill_editing: false,
            session_tools: false,
        };
        let ctx = ToolDefsContext {
            is_base: false,
            groups: &groups,
            default_timezone: "UTC",
            has_sandbox: true,
            browser_renderer_url: "disabled",
            lsp_enabled: false,
        };
        let tools = build_internal_tool_definitions(&ctx);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"workspace_write"));
        assert!(names.contains(&"code_exec")); // sandbox version
        assert!(!names.contains(&"process"));
        assert!(!names.contains(&"git"));
        assert!(!names.contains(&"session"));
        assert!(!names.contains(&"tool_create"));
        assert!(!names.contains(&"browser_action"));
    }

    #[test]
    fn filter_for_subagent_excludes_denied() {
        let tools = vec![
            ToolDefinition {
                name: "workspace_write".to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            },
            ToolDefinition {
                name: "cron".to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            },
        ];
        let denied: Vec<String> = vec!["cron".to_string()];
        let filtered = filter_for_subagent(tools, &denied, None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "workspace_write");
    }

    #[test]
    fn all_system_tool_names_has_expected_count() {
        // Update this number when intentionally adding/removing tools.
        // Catches accidental gating that would silently shrink the list.
        let actual = all_system_tool_names().len();
        assert!(
            actual >= 29,
            "expected >= 29 tools in all_system_tool_names(), got {actual}"
        );
    }

    #[test]
    fn all_system_tool_names_includes_known_tools() {
        let names = all_system_tool_names();
        for expected in [
            "agent", "memory", "workspace_write", "workspace_read",
            "code_exec", "process", "tool_create", "git", "session",
            "web_fetch", "skill", "browser_action",
        ] {
            assert!(
                names.contains(&expected),
                "{expected:?} missing from all_system_tool_names()"
            );
        }
    }

    /// Pin the agent tool schema after the run/message/collect → ask merger.
    /// Catches accidental reintroduction of the old action enum or params.
    #[test]
    fn agent_action_enum_is_three_values() {
        let groups = ToolGroups {
            git: false,
            tool_management: false,
            skill_editing: false,
            session_tools: false,
        };
        let ctx = ToolDefsContext {
            is_base: false,
            groups: &groups,
            default_timezone: "UTC",
            has_sandbox: true,
            browser_renderer_url: "disabled",
            lsp_enabled: false,
        };
        let tools = build_internal_tool_definitions(&ctx);
        let agent = tools
            .iter()
            .find(|t| t.name == "agent")
            .expect("agent tool present");

        let actions = agent
            .input_schema
            .pointer("/properties/action/enum")
            .and_then(|v| v.as_array())
            .expect("action enum present");
        let names: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(
            names,
            vec!["ask", "status", "kill"],
            "agent action enum drift — must be exactly ['ask', 'status', 'kill']"
        );

        // Removed params must not be present.
        let props = agent
            .input_schema
            .pointer("/properties")
            .and_then(|v| v.as_object())
            .expect("agent properties");
        for removed in ["task", "mode", "wait_for_result"] {
            assert!(
                !props.contains_key(removed),
                "{removed:?} must be removed from agent tool schema"
            );
        }
        // New params must be present.
        for required in ["target", "text", "fresh"] {
            assert!(
                props.contains_key(required),
                "{required:?} must exist in agent tool schema"
            );
        }
    }

    #[test]
    fn static_core_is_exactly_ten_tools() {
        let names: Vec<&'static str> = static_core_tool_names().into();
        assert_eq!(names.len(), 10, "static core must be exactly 10 tools (spec)");

        let expected = [
            "workspace_read", "workspace_write", "workspace_edit", "workspace_list",
            "code_exec", "memory", "agent", "skill_use", "web_fetch", "tool_use",
        ];
        for name in expected {
            assert!(names.contains(&name), "static core missing expected tool: {name}");
        }
    }
}
