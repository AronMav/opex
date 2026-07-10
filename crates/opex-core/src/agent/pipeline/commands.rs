//! Pipeline step: commands — slash commands /status, /clear, etc. (migrated from engine_commands.rs).
//!
//! Free function `handle_command` takes a `CommandContext` struct instead of `&self` on `AgentEngine`.

use anyhow::Result;
use std::future::Future;
use std::sync::atomic::{AtomicU8, Ordering};

use opex_types::{IncomingMessage, Message, MessageRole};
use crate::agent::commands::spec::CommandOutcome;
use crate::agent::history;
use crate::agent::localization;
use crate::agent::memory_service::MemoryService;
use crate::agent::providers::LlmProvider;
use crate::config::CompactionConfig;
use crate::db::sessions;

/// Parsed `/rollback` argument.
#[derive(Debug)]
pub enum RollbackCmd {
    List,
    To(usize),
    Diff(usize),
    File(usize, String),
}

/// Parse a `/rollback` argument into a `RollbackCmd`.
/// Unknown/garbage input falls back to `List` (safe default).
pub fn parse_rollback_command(arg: &str) -> RollbackCmd {
    let a = arg.trim();
    if a.is_empty() || a == "list" {
        return RollbackCmd::List;
    }
    if let Some(rest) = a.strip_prefix("diff ") {
        if let Ok(n) = rest.trim().parse::<usize>() {
            return RollbackCmd::Diff(n);
        }
        return RollbackCmd::List;
    }
    // "N file <path>"
    let mut it = a.split_whitespace();
    if let Some(first) = it.next()
        && let Ok(n) = first.parse::<usize>()
    {
        match it.next() {
            Some("file") => {
                let path = it.collect::<Vec<_>>().join(" ");
                if !path.is_empty() {
                    return RollbackCmd::File(n, path);
                }
                return RollbackCmd::List;
            }
            None => return RollbackCmd::To(n),
            _ => return RollbackCmd::List,
        }
    }
    RollbackCmd::List
}

/// Parsed `/voice` argument.
pub enum VoiceCmd {
    Status,
    Set(&'static str),
}

/// Map a `/voice` argument to an action. Unknown args fall back to `Status`.
pub fn parse_voice_command(arg: &str) -> VoiceCmd {
    match arg.trim().to_lowercase().as_str() {
        "on" => VoiceCmd::Set("on"),
        "off" => VoiceCmd::Set("off"),
        _ => VoiceCmd::Status,
    }
}

// ── CommandContext ──────────────────────────────────────────────────────────

/// All dependencies needed by slash command handlers, passed explicitly instead of via `&self`.
pub struct CommandContext<'a> {
    pub agent_name: &'a str,
    pub agent_language: &'a str,
    pub agent_model: &'a str,
    pub dm_scope: &'a str,
    pub max_history_messages: Option<usize>,
    pub compaction_config: Option<&'a CompactionConfig>,
    pub db: &'a sqlx::PgPool,
    pub provider: &'a dyn LlmProvider,
    pub compaction_provider: Option<&'a dyn LlmProvider>,
    pub thinking_level: &'a AtomicU8,
    pub memory_store: &'a dyn MemoryService,
    /// Owned engine `Arc` (resolved from `state.self_ref`) so `/goal` can spawn a
    /// background driver that outlives the request. `None` when no self-ref is set.
    pub engine_arc: Option<std::sync::Arc<crate::agent::engine::AgentEngine>>,
    /// Toolgate base URL, when configured. Used by `/help` to append the live
    /// handler-command section. `None` disables the append (no regression —
    /// `/help` falls back to the static localized text).
    pub toolgate_url: Option<String>,
    /// HTTP client used to fetch handler manifests for `/help`. `None` when
    /// `toolgate_url` is also `None`.
    pub http: Option<reqwest::Client>,
}

/// Spawn (replacing any existing) the goal driver for a session. Returns false when
/// no engine ref / goal pool is available.
fn start_goal_driver(
    ctx: &CommandContext<'_>,
    session_id: uuid::Uuid,
    target: crate::agent::goal::pool::GoalTarget,
) -> bool {
    let Some(engine) = ctx.engine_arc.clone() else {
        return false;
    };
    let Some(pool) = engine.cfg().goal_pool.clone() else {
        return false;
    };
    crate::agent::goal::pool::stop(&pool, session_id);
    let handle = crate::agent::goal::driver::spawn_goal_driver(engine, session_id, target);
    pool.insert(session_id, handle);
    true
}

fn stop_goal_driver(ctx: &CommandContext<'_>, session_id: uuid::Uuid) {
    if let Some(engine) = ctx.engine_arc.as_ref()
        && let Some(pool) = engine.cfg().goal_pool.clone()
    {
        crate::agent::goal::pool::stop(&pool, session_id);
    }
}

/// Append a "file handler commands" section to the base `/help` text.
///
/// Pure/no-IO so it's cheap to unit-test in isolation from the live
/// `HandlerRegistry` fetch. Returns `base` unchanged when `handlers` is empty
/// (no regression to the static localized `/help`).
fn append_handlers_section(
    base: &str,
    header: &str,
    handlers: &[crate::agent::commands::spec::CommandSpec],
) -> String {
    if handlers.is_empty() {
        return base.to_string();
    }
    let mut out = base.to_string();
    out.push_str("\n\n");
    out.push_str(header);
    out.push('\n');
    for h in handlers {
        out.push_str(&format!("/{} — {}\n", h.name, h.description));
    }
    out
}

// ── handle_command ─────────────────────────────────────────────────────────

/// Имена, реально обрабатываемые `match` в `handle_command` (без ведущего `/`).
/// Держать синхронно с ветками ниже; drift-гард-тест сверяет с BUILTIN_NAMES.
// consumed by the registry/dispatch drift-guard test below (`dispatch_names_match_registry_builtins`);
// production match arms are exercised directly, not via this list.
#[allow(dead_code)]
pub const DISPATCH_NAMES: &[&str] = &[
    "status", "new", "reset", "compact", "rollback", "model", "think",
    "voice", "usage", "export", "help", "memory", "goal", "subgoal",
];

/// Handle /slash commands. Returns `Some(result)` if a command matched, `None` otherwise.
///
/// Two callbacks are required for operations that still live on `AgentEngine`:
/// - `invalidate_cache_fn`: called after `/model` changes to invalidate the YAML tools cache.
pub async fn handle_command<F, Fut>(
    ctx: &CommandContext<'_>,
    text: &str,
    msg: &IncomingMessage,
    invalidate_cache_fn: F,
) -> Option<Result<CommandOutcome>>
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    let cmd = text.trim();
    if !cmd.starts_with('/') {
        return None;
    }
    let (raw_command, args) = cmd.split_once(' ').unwrap_or((cmd, ""));
    // Strip @botname suffix (Telegram sends /status@my_bot)
    let command = raw_command.split('@').next().unwrap_or(raw_command);
    tracing::debug!(command = %command, raw = %raw_command, "slash command received");

    // T03 triage Point 5: scope session lookups by chat, not just by
    // platform, so /status, /new, /reset, etc. act on the session for THIS
    // chat/group, not whichever chat the same user_id last touched.
    let chat_scope = msg.chat_scope();

    let s = localization::get_strings(ctx.agent_language);

    match command {
        "/status" => {
            let session_info = match sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            ).await {
                Ok(Some(sid)) => {
                    let count = sessions::count_messages(ctx.db, sid).await.unwrap_or(0);
                    localization::fmt(s.status_session_active, &[&count.to_string()])
                }
                _ => s.status_session_none.to_string(),
            };
            let chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks WHERE agent_id = $1")
                .bind(ctx.agent_name)
                .fetch_one(ctx.db).await.unwrap_or(0);
            let provider_name = ctx.provider.name();
            let current_model = ctx.provider.current_model();
            Some(Ok(CommandOutcome::Text(
                localization::fmt(s.status_format, &[ctx.agent_name, provider_name, &current_model, &session_info, &chunks.to_string()])
            )))
        }
        "/new" => {
            match sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            ).await {
                Ok(Some(sid)) => {
                    if let Err(e) = sessions::delete_session(ctx.db, sid).await {
                        return Some(Err(e));
                    }
                    Some(Ok(CommandOutcome::Text(s.new_session_started.to_string())))
                }
                Ok(None) => Some(Ok(CommandOutcome::Text(s.new_session_none.to_string()))),
                Err(e) => Some(Err(e)),
            }
        }
        "/reset" => {
            // Delete session
            if let Ok(Some(sid)) = sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            ).await {
                let _ = sessions::delete_session(ctx.db, sid).await;
            }
            // Delete this agent's unpinned memory (scoped by agent_id)
            let deleted: i64 = sqlx::query_scalar(
                "WITH d AS (DELETE FROM memory_chunks WHERE pinned = false AND agent_id = $1 RETURNING 1) SELECT COUNT(*) FROM d"
            ).bind(ctx.agent_name).fetch_one(ctx.db).await.unwrap_or(0);
            Some(Ok(CommandOutcome::Text(localization::fmt(s.reset_done, &[&deleted.to_string()]))))
        }
        "/compact" => {
            let sid = match sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            ).await {
                Ok(Some(sid)) => sid,
                _ => return Some(Ok(CommandOutcome::Text(s.compact_no_session.to_string()))),
            };
            let history_rows = match sessions::load_messages(ctx.db, sid, Some(ctx.max_history_messages.unwrap_or(50) as i64)).await {
                Ok(h) => h,
                Err(e) => return Some(Err(e)),
            };
            let mut messages: Vec<Message> = history_rows.into_iter().map(|m| Message {
                role: match m.role.as_str() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    _ => MessageRole::System,
                },
                content: m.content,
                tool_calls: m.tool_calls.and_then(|tc| {
                    serde_json::from_value::<Vec<opex_types::ToolCall>>(tc).ok()
                }),
                tool_call_id: m.tool_call_id.map(opex_types::ids::ToolCallId::from),
                thinking_blocks: vec![],
            db_id: None,
            }).collect();
            let before = messages.len();
            let preserve = ctx.compaction_config
                .map(|c| c.preserve_last_n as usize).unwrap_or(10);
            let messages_snapshot = messages.clone();
            let mut compact_result = None;
            for attempt in 0..2u8 {
                match history::compact_if_needed(
                    &mut messages, ctx.provider, ctx.compaction_provider, 0, preserve, Some(ctx.agent_language),
                ).await {
                    Ok(r) => { compact_result = Some(r); break; }
                    Err(e) if attempt == 0 => {
                        tracing::warn!(error = %e, "compaction failed, retrying...");
                        messages = messages_snapshot.clone();
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    Err(e) => {
                        return Some(Ok(CommandOutcome::Text(format!("Compaction failed after retry: {}", e))));
                    }
                }
            }
            match compact_result {
                Some(Some(facts)) => {
                    let after = messages.len();
                    // Call extracted pipeline::memory function directly
                    super::memory::index_facts_to_memory(ctx.memory_store, ctx.agent_name, &facts).await;

                    // Persist compacted messages to DB (atomic transaction)
                    if let Err(e) = async {
                        let mut tx = ctx.db.begin().await?;
                        sqlx::query("DELETE FROM messages WHERE session_id = $1")
                            .bind(sid)
                            .execute(&mut *tx)
                            .await?;
                        for m in &messages {
                            let role = match m.role {
                                MessageRole::User => "user",
                                MessageRole::Assistant => "assistant",
                                MessageRole::System => "system",
                                MessageRole::Tool => "tool",
                            };
                            let tc_json = m.tool_calls.as_ref()
                                .and_then(|tc| serde_json::to_value(tc).map_err(|e| {
                                    tracing::warn!(error = %e, "failed to serialize tool_calls during compact");
                                    e
                                }).ok());
                            sqlx::query(
                                "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id) \
                                 VALUES ($1, $2, $3, $4, $5)",
                            )
                            .bind(sid)
                            .bind(role)
                            .bind(&m.content)
                            .bind(tc_json.as_ref())
                            .bind(m.tool_call_id.as_ref().map(|id| id.as_str()))
                            .execute(&mut *tx)
                            .await?;
                        }
                        tx.commit().await?;
                        Ok::<(), anyhow::Error>(())
                    }.await {
                        return Some(Ok(CommandOutcome::Text(format!("Compaction succeeded but DB persist failed: {}", e))));
                    }

                    Some(Ok(CommandOutcome::Text(
                        localization::fmt(s.compact_done, &[&before.to_string(), &after.to_string(), &facts.len().to_string()])
                    )))
                }
                Some(None) => Some(Ok(CommandOutcome::Text(s.compact_not_needed.to_string()))),
                None => Some(Ok(CommandOutcome::Text("Compaction failed.".to_string()))),
            }
        }
        "/rollback" => {
            let Some(engine) = ctx.engine_arc.clone() else {
                return Some(Ok(CommandOutcome::Text("Откат недоступен в этом контексте.".to_string())));
            };
            let Some(cm) = engine.cfg().checkpoint_manager.clone() else {
                return Some(Ok(CommandOutcome::Text("Чекпойнты отключены.".to_string())));
            };
            if !cm.enabled() {
                return Some(Ok(CommandOutcome::Text("Чекпойнты отключены.".to_string())));
            }
            let ws = engine.cfg().workspace_dir.clone();
            let agent = engine.cfg().agent.name.clone();
            let cmd = parse_rollback_command(args);
            let result = match cmd {
                RollbackCmd::List => match cm.list_checkpoints(&agent).await {
                    Ok(list) if list.is_empty() => "Чекпойнтов нет.".to_string(),
                    Ok(list) => {
                        let mut s = String::from("Чекпойнты (свежие сверху):\n");
                        for c in list.iter().take(30) {
                            let short = &c.commit.get(..8).unwrap_or(&c.commit);
                            s.push_str(&format!("  {}. {} ({})  {}\n", c.n, c.created, short, c.summary));
                        }
                        s.push_str("\n`/rollback N` — откат · `/rollback diff N` — показать · `/rollback N file <путь>` — один файл");
                        s
                    }
                    Err(e) => format!("Ошибка списка чекпойнтов: {e}"),
                },
                RollbackCmd::Diff(n) => match cm.diff(&agent, &ws, n).await {
                    Ok(d) if d.trim().is_empty() => format!("Чекпойнт {n}: отличий нет."),
                    Ok(d) => {
                        let body: String = d.lines().take(200).collect::<Vec<_>>().join("\n");
                        format!("Diff против чекпойнта {n}:\n```diff\n{body}\n```")
                    }
                    Err(e) => format!("Ошибка diff: {e}"),
                },
                RollbackCmd::To(n) => match cm.restore(&agent, &ws, n, None).await {
                    Ok(rep) => format!(
                        "Откат к чекпойнту {} выполнен ({} файлов). Текущее состояние сохранено{}.",
                        rep.n,
                        rep.files.len(),
                        rep.new_checkpoint.map(|c| format!(" как чекпойнт {c}")).unwrap_or_default(),
                    ),
                    Err(e) => format!("Ошибка отката: {e}"),
                },
                RollbackCmd::File(n, path) => match cm.restore(&agent, &ws, n, Some(&path)).await {
                    Ok(_) => format!("Файл `{path}` восстановлен из чекпойнта {n}."),
                    Err(e) => format!("Ошибка отката файла: {e}"),
                },
            };
            Some(Ok(CommandOutcome::Text(result)))
        }
        "/model" => {
            let model_arg = args.trim();
            if model_arg.is_empty() || model_arg == "status" {
                let current = ctx.provider.current_model();
                let base = ctx.agent_model;
                if current == *base {
                    Some(Ok(CommandOutcome::Text(localization::fmt(s.model_current, &[&current]))))
                } else {
                    Some(Ok(CommandOutcome::Text(
                        localization::fmt(s.model_override, &[&current, base])
                    )))
                }
            } else if model_arg == "reset" {
                ctx.provider.set_model_override(None);
                invalidate_cache_fn().await;
                Some(Ok(CommandOutcome::Text(localization::fmt(s.model_reset, &[ctx.agent_model]))))
            } else {
                ctx.provider.set_model_override(Some(model_arg.to_string()));
                invalidate_cache_fn().await;
                Some(Ok(CommandOutcome::Text(localization::fmt(s.model_switched, &[model_arg]))))
            }
        }
        "/think" => {
            let arg = args.trim();
            let current = ctx.thinking_level.load(Ordering::Relaxed);
            let new_level: u8 = match arg {
                "off" | "0" | "false" | "нет" => 0,
                "on" | "true" | "да" => 3,
                "minimal" | "min" | "1" => 1,
                "low" | "2" => 2,
                "medium" | "med" | "3" => 3,
                "high" | "4" => 4,
                "max" | "xhigh" | "5" => 5,
                _ => if current == 0 { 3 } else { 0 }, // toggle
            };
            ctx.thinking_level.store(new_level, Ordering::Relaxed);
            let label = match new_level {
                0 => "OFF",
                1 => "MINIMAL",
                2 => "LOW",
                3 => "MEDIUM",
                4 => "HIGH",
                5 => "MAX",
                _ => "?",
            };
            Some(Ok(CommandOutcome::Text(
                localization::fmt(s.think_level, &[label, &new_level.to_string()])
            )))
        }
        "/voice" => {
            let chat_id = msg
                .context
                .get("chat_id")
                .map(|v| v.to_string().trim_matches('"').to_string())
                .filter(|c| !c.is_empty() && c != "null");
            let Some(chat_id) = chat_id else {
                return Some(Ok(CommandOutcome::Text(
                    "/voice only applies to chat channels (Telegram, etc.).".to_string(),
                )));
            };
            let channel = msg.channel.as_str();
            match parse_voice_command(args) {
                VoiceCmd::Set(mode) => {
                    if let Err(e) =
                        crate::db::channel_voice_modes::set_voice_mode(ctx.db, channel, &chat_id, mode).await
                    {
                        return Some(Ok(CommandOutcome::Text(format!("Failed to set voice mode: {e}"))));
                    }
                    let reply = if mode == "on" {
                        "Voice replies enabled for this chat. Each reply will also be sent as audio. /voice off to disable."
                    } else {
                        "Voice replies disabled for this chat."
                    };
                    Some(Ok(CommandOutcome::Text(reply.to_string())))
                }
                VoiceCmd::Status => {
                    let mode = crate::db::channel_voice_modes::get_voice_mode(ctx.db, channel, &chat_id)
                        .await
                        .unwrap_or_else(|_| "off".to_string());
                    Some(Ok(CommandOutcome::Text(format!(
                        "Voice mode for this chat: {mode}. Use /voice on or /voice off."
                    ))))
                }
            }
        }
        "/usage" => {
            let session_id = match sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            ).await {
                Ok(Some(sid)) => Some(sid),
                _ => None,
            };

            // Session usage
            let session_stats = if let Some(sid) = session_id {
                sqlx::query_as::<_, (i64, i64, i64)>(
                    "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COUNT(*) \
                     FROM usage_log WHERE session_id = $1"
                )
                .bind(sid)
                .fetch_optional(ctx.db)
                .await
                .ok()
                .flatten()
            } else {
                None
            };

            // Today's agent usage
            let today_stats = sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COUNT(*) \
                 FROM usage_log WHERE agent_id = $1 AND created_at > CURRENT_DATE"
            )
            .bind(ctx.agent_name)
            .fetch_optional(ctx.db)
            .await
            .ok()
            .flatten()
            .unwrap_or((0, 0, 0));

            let mut out = localization::fmt(s.usage_header, &[ctx.agent_name, &today_stats.0.to_string(), &today_stats.1.to_string(), &today_stats.2.to_string()]);

            if let Some((s_inp, s_out, s_calls)) = session_stats {
                out.push('\n');
                out.push_str(
                    &localization::fmt(s.usage_session, &[&s_inp.to_string(), &s_out.to_string(), &s_calls.to_string()])
                );
            }

            Some(Ok(CommandOutcome::Text(out)))
        }
        "/export" => {
            let sid = match sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            ).await {
                Ok(Some(sid)) => sid,
                _ => return Some(Ok(CommandOutcome::Text(s.export_no_session.to_string()))),
            };
            let rows = match sessions::load_messages(ctx.db, sid, Some(500)).await {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            if rows.is_empty() {
                return Some(Ok(CommandOutcome::Text(s.export_empty.to_string())));
            }
            let mut out = localization::fmt(s.export_header, &[ctx.agent_name, &sid.to_string()]);
            for m in &rows {
                let role = match m.role.as_str() {
                    "user" => "👤 User",
                    "assistant" => "🤖 Assistant",
                    "system" => "⚙️ System",
                    "tool" => "🔧 Tool",
                    _ => &m.role,
                };
                let time = m.created_at.format("%H:%M");
                let content = if m.content.chars().count() > 500 {
                    format!("{}...", m.content.chars().take(500).collect::<String>())
                } else {
                    m.content.clone()
                };
                out.push_str(&format!("\n**{role}** ({time}):\n{content}\n"));
            }
            Some(Ok(CommandOutcome::Text(out)))
        }
        "/help" | "/commands" => {
            let mut out = s.help_text.to_string();
            if let (Some(toolgate_url), Some(http)) = (ctx.toolgate_url.clone(), ctx.http.clone()) {
                let reg = crate::agent::handler_registry::HandlerRegistry::new(toolgate_url, http);
                reg.refresh().await;
                let manifests = reg.manifests().await;
                let enabled = crate::agent::fse::get_enabled_allowlist(ctx.db).await;
                let handlers = crate::agent::commands::handler_source::derive_handler_commands(
                    &manifests, &enabled, ctx.agent_language,
                );
                out = append_handlers_section(&out, s.handlers_header, &handlers);
            }
            Some(Ok(CommandOutcome::Text(out)))
        }
        "/memory" => {
            let query = args.trim();
            let (results, mode) = if query.is_empty() {
                match ctx.memory_store.recent(10).await {
                    Ok(r) => (r, "recent".to_string()),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                match ctx.memory_store.search(query, 8, &[], ctx.agent_name).await {
                    Ok((r, m)) => (r, m),
                    Err(e) => return Some(Err(e)),
                }
            };
            if results.is_empty() {
                return Some(Ok(CommandOutcome::Text(s.memory_empty.to_string())));
            }
            let lines: Vec<String> = results.iter().enumerate().map(|(i, r)| {
                let pin = if r.pinned { "📌 " } else { "" };
                format!("{}{}. {}", pin, i + 1,
                    r.content.chars().take(200).collect::<String>())
            }).collect();
            Some(Ok(CommandOutcome::Text(
                localization::fmt(s.memory_header, &[&mode, &results.len().to_string(), &lines.join("\n\n")])
            )))
        }
        "/goal" => {
            use crate::agent::goal::{parse_goal_command, GoalCmd};
            let session_id = match sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            )
            .await
            {
                Ok(Some(sid)) => sid,
                _ => return Some(Ok(CommandOutcome::Text("No active session for this chat.".to_string()))),
            };
            // Channel sessions carry a chat_id; web sessions don't (driver delivers via ui_event).
            let target: crate::agent::goal::pool::GoalTarget = msg
                .context
                .get("chat_id")
                .and_then(|v| v.as_i64())
                .map(|cid| (msg.channel.clone(), cid));
            match parse_goal_command(args) {
                GoalCmd::Set(text) => {
                    let max_turns = 20;
                    if let Err(e) = crate::db::session_goals::upsert(ctx.db, session_id, &text, max_turns).await {
                        return Some(Ok(CommandOutcome::Text(format!("Failed to set goal: {e}"))));
                    }
                    if !start_goal_driver(ctx, session_id, target) {
                        return Some(Ok(CommandOutcome::Text("Goal saved, but the autonomous driver could not start here.".to_string())));
                    }
                    Some(Ok(CommandOutcome::Text(format!(
                        "🎯 Goal set: {text}\nWorking on it autonomously (max {max_turns} turns). /goal status · /goal pause · /goal clear."
                    ))))
                }
                GoalCmd::Status => match crate::db::session_goals::get(ctx.db, session_id).await {
                    Ok(Some(g)) => Some(Ok(CommandOutcome::Text(format!(
                        "🎯 Goal: {}\nStatus: {} ({}/{} turns){}",
                        g.goal_text,
                        g.status,
                        g.turn_count,
                        g.max_turns,
                        if g.subgoals.is_empty() {
                            String::new()
                        } else {
                            format!("\nSubgoals: {}", g.subgoals.join("; "))
                        }
                    )))),
                    _ => Some(Ok(CommandOutcome::Text("No goal set. Use /goal <text>.".to_string()))),
                },
                GoalCmd::Pause => {
                    let _ = crate::db::session_goals::set_status(ctx.db, session_id, "paused").await;
                    stop_goal_driver(ctx, session_id);
                    Some(Ok(CommandOutcome::Text("⏸ Goal paused. /goal resume to continue.".to_string())))
                }
                GoalCmd::Resume => {
                    let _ = crate::db::session_goals::set_status(ctx.db, session_id, "active").await;
                    if !start_goal_driver(ctx, session_id, target) {
                        return Some(Ok(CommandOutcome::Text("Could not resume the autonomous driver here.".to_string())));
                    }
                    Some(Ok(CommandOutcome::Text("▶ Goal resumed.".to_string())))
                }
                GoalCmd::Clear => {
                    stop_goal_driver(ctx, session_id);
                    let _ = crate::db::session_goals::clear(ctx.db, session_id).await;
                    Some(Ok(CommandOutcome::Text("🗑 Goal cleared.".to_string())))
                }
            }
        }
        "/subgoal" => {
            use crate::agent::goal::{parse_subgoal_command, SubgoalCmd};
            let session_id = match sessions::find_active_session(
                ctx.db, ctx.agent_name, &msg.user_id, &msg.channel, ctx.dm_scope, chat_scope.as_deref(),
            )
            .await
            {
                Ok(Some(sid)) => sid,
                _ => return Some(Ok(CommandOutcome::Text("No active session.".to_string()))),
            };
            let Ok(Some(mut g)) = crate::db::session_goals::get(ctx.db, session_id).await else {
                return Some(Ok(CommandOutcome::Text("No active goal — set one with /goal <text>.".to_string())));
            };
            match parse_subgoal_command(args) {
                SubgoalCmd::Add(t) => {
                    g.subgoals.push(t);
                    let _ = crate::db::session_goals::set_subgoals(ctx.db, session_id, &g.subgoals).await;
                    Some(Ok(CommandOutcome::Text(format!("Added subgoal. {} total.", g.subgoals.len()))))
                }
                SubgoalCmd::List => {
                    if g.subgoals.is_empty() {
                        Some(Ok(CommandOutcome::Text("No subgoals.".to_string())))
                    } else {
                        Some(Ok(CommandOutcome::Text(g
                            .subgoals
                            .iter()
                            .enumerate()
                            .map(|(i, s)| format!("{}. {s}", i + 1))
                            .collect::<Vec<_>>()
                            .join("\n"))))
                    }
                }
                SubgoalCmd::Remove(n) => {
                    if n >= 1 && n <= g.subgoals.len() {
                        g.subgoals.remove(n - 1);
                        let _ = crate::db::session_goals::set_subgoals(ctx.db, session_id, &g.subgoals).await;
                        Some(Ok(CommandOutcome::Text(format!("Removed subgoal {n}. {} left.", g.subgoals.len()))))
                    } else {
                        Some(Ok(CommandOutcome::Text(format!("No subgoal #{n}."))))
                    }
                }
            }
        }
        _ => None, // Unknown command — pass to LLM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_parse() {
        use super::{parse_rollback_command, RollbackCmd};
        assert!(matches!(parse_rollback_command(""), RollbackCmd::List));
        assert!(matches!(parse_rollback_command("list"), RollbackCmd::List));
        assert!(matches!(parse_rollback_command("2"), RollbackCmd::To(2)));
        assert!(matches!(parse_rollback_command("diff 3"), RollbackCmd::Diff(3)));
        match parse_rollback_command("2 file notes/x.md") {
            RollbackCmd::File(2, p) => assert_eq!(p, "notes/x.md"),
            other => panic!("unexpected: {other:?}"),
        }
        // мусор → List (безопасный дефолт)
        assert!(matches!(parse_rollback_command("garbage"), RollbackCmd::List));
    }

    #[test]
    fn goal_and_subgoal_parsers() {
        use crate::agent::goal::{parse_goal_command, parse_subgoal_command, GoalCmd, SubgoalCmd};
        assert!(matches!(parse_goal_command("pause"), GoalCmd::Pause));
        assert!(matches!(parse_subgoal_command("remove 1"), SubgoalCmd::Remove(1)));
    }

    #[test]
    fn parse_voice_command_maps_args() {
        assert!(matches!(parse_voice_command("on"), VoiceCmd::Set("on")));
        assert!(matches!(parse_voice_command("off"), VoiceCmd::Set("off")));
        assert!(matches!(parse_voice_command(""), VoiceCmd::Status));
        assert!(matches!(parse_voice_command("status"), VoiceCmd::Status));
        assert!(matches!(parse_voice_command("garbage"), VoiceCmd::Status));
    }

    #[test]
    fn append_handlers_section_appends_lines_and_preserves_base_when_empty() {
        use crate::agent::commands::spec::{
            ArgType, CommandArg, CommandCategory, CommandScope, CommandSourceKind, CommandSpec, Visibility,
        };

        let base = "📋 *Available commands:*\n\n/status — agent status";

        // Empty handler list → base returned verbatim (no regression).
        assert_eq!(append_handlers_section(base, "🧩 *File handlers:*", &[]), base);

        let handlers = vec![CommandSpec {
            name: "summarize_video".to_string(),
            aliases: vec![],
            description: "Summarize a video into notes".to_string(),
            category: CommandCategory::Media,
            scope: CommandScope::Both,
            args: vec![CommandArg {
                name: "source".to_string(),
                description: "url or file".to_string(),
                arg_type: ArgType::String,
                required: false,
                choices: None,
                capture_remaining: true,
                menu: false,
            }],
            visibility: Visibility::All,
            source: CommandSourceKind::Handler { handler_id: "summarize_video".to_string() },
        }];
        let out = append_handlers_section(base, "🧩 *File handlers:*", &handlers);
        assert!(out.starts_with(base), "base text must be preserved verbatim");
        assert!(out.contains("🧩 *File handlers:*"));
        assert!(out.contains("/summarize_video — Summarize a video into notes"));
    }

    #[test]
    fn dispatch_names_match_registry_builtins() {
        use crate::agent::commands::builtin::BUILTIN_NAMES;
        let mut dispatch: Vec<&str> = super::DISPATCH_NAMES.to_vec();
        let mut builtin: Vec<&str> = BUILTIN_NAMES.to_vec();
        dispatch.sort_unstable();
        builtin.sort_unstable();
        assert_eq!(dispatch, builtin,
            "match-диспетч и BUILTIN_NAMES разъехались — обновите обе стороны");
    }
}
