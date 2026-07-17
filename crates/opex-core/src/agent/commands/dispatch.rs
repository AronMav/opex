//! Task 5: dispatch handler commands (`/summarize_video <url>`, `/transcribe`,
//! …) — the typed-command counterpart of the model-driven `file_handler` tool
//! (`agent/tool_handlers/file_handler.rs`). Called from
//! `engine::context_builder::handle_command` AFTER the builtin `match` in
//! `pipeline::commands::handle_command` returns `None` (i.e. `text` isn't one
//! of the 14 builtin slash commands).
//!
//! Flow: parse `/name args` → is `name` a live ASYNC handler id? → resolve a
//! source (explicit url/path arg, else the session's most-recent upload) →
//! trust-gate the resolved handler against that source (same matcher +
//! allowlist the composer/menu paths use) → `insert_handler_job`.
//!
//! Only async handlers are ever dispatched here (F070 parity): sync handlers
//! (describe/extract_document/save) run inline via `/api/files/run` and
//! would strand on this queue (no `/complete` callback is ever posted for
//! them).

use anyhow::Result;
use opex_types::IncomingMessage;
use uuid::Uuid;

use super::handler_source::is_valid_command_token;
use super::spec::CommandOutcome;
use crate::agent::handler_registry::{
    HandlerManifest, HandlerRegistry, match_buttons, match_url_handlers, retain_async_handlers,
};

/// Dependencies for handler-command dispatch, built by the engine wrapper
/// (`context_builder.rs::handle_command`) from `AgentEngine::cfg()`. Deliberately
/// does NOT carry a pre-resolved `session_id` — `try_handler_command` resolves
/// it itself via `find_active_session` (mirroring how the builtin commands in
/// `pipeline::commands` resolve theirs), so a message with no active session
/// falls through to ordinary (non-command) processing instead of erroring.
pub struct HandlerDispatchDeps<'a> {
    pub db: &'a sqlx::PgPool,
    /// Shared registry (`AgentConfig::handler_registry`) — `refresh()` here is
    /// a conditional GET against the process-wide ETag cache, not a fresh
    /// fetch (Task 1, perf).
    pub handlers: &'a HandlerRegistry,
    pub agent_name: &'a str,
    pub agent_language: &'a str,
    pub dm_scope: &'a str,
}

/// Parse `/name rest-of-args` into `(name, args)`. `name` is lowercased and
/// stripped of an `@botname` suffix (Telegram sends `/status@my_bot`); `args`
/// is trimmed. Returns `None` if `text` doesn't start with `/` or the name is
/// empty (e.g. bare `/`).
pub fn parse_command_line(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let rest = t.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let name = name.split('@').next().unwrap_or(name);
    if name.is_empty() {
        return None;
    }
    Some((name.to_lowercase(), args.trim().to_string()))
}

/// Resolve a parsed command `name` to the target handler id.
///
/// `derive_handler_commands` advertises a handler under its `<command>`
/// override name/aliases when present (falling back to the handler id when
/// there's no override, or the override `name` is invalid). Dispatch must
/// agree with that: a name typed by the user can be either the raw handler
/// id (still live for handlers without an override, or as a fallback) OR a
/// valid override name/alias — never a bare substring match, and never an
/// alias `derive_handler_commands` itself dropped as invalid.
///
/// Only `execution == "async"` handlers are candidates (sync handlers never
/// become dispatchable commands here — see module doc). Returns `None` when
/// no manifest resolves, so the caller falls through to ordinary (non-command)
/// processing exactly as it does today for an unrecognized `/name`.
fn resolve_command_to_handler_id(manifests: &[HandlerManifest], name: &str) -> Option<String> {
    // `name` arrives already lowercased from `parse_command_line`, while
    // `derive_handler_commands` advertises the id/override verbatim (an override
    // name may legally contain uppercase per `is_valid_command_token`). Compare
    // case-insensitively so an advertised `/SumVid` is actually dispatchable —
    // otherwise the command is dead (advertised but never matches).
    manifests
        .iter()
        .filter(|m| m.execution == "async")
        .find(|m| {
            m.id.eq_ignore_ascii_case(name)
                || m.command.as_ref().is_some_and(|ov| {
                    is_valid_command_token(&ov.name)
                        && (ov.name.eq_ignore_ascii_case(name)
                            || ov
                                .aliases
                                .iter()
                                .filter(|a| is_valid_command_token(a))
                                .any(|a| a.eq_ignore_ascii_case(name)))
                })
        })
        .map(|m| m.id.clone())
}

/// First operator valve (in `HandlerManifest.config`) that declares
/// `choices` (MVP: only the first choice-valve per command is honored — a
/// handler with multiple choice-valves gets a menu for the first one only).
pub(crate) fn first_choice_valve(m: &HandlerManifest) -> Option<(String, Vec<String>)> {
    m.config.as_array()?.iter().find_map(|f| {
        let name = f.get("name")?.as_str()?.to_string();
        let choices: Vec<String> = f
            .get("choices")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        (!choices.is_empty()).then_some((name, choices))
    })
}

/// Dispatch a `/name [args]` command that the builtin registry didn't handle.
///
/// Returns `None` when:
/// - `text` isn't a `/command` at all, OR
/// - `name` doesn't match any currently-live ASYNC handler id, OR
/// - there's no active session for this chat yet.
///
/// In all three cases the caller falls through to ordinary message
/// processing (LLM turn / session bootstrap). Otherwise returns `Some(Ok(_))`
/// — either an enqueue confirmation, a rejection ("not available for this
/// source"), or an `argsMenu` prompting for a source.
pub async fn try_handler_command(
    deps: &HandlerDispatchDeps<'_>,
    text: &str,
    msg: &IncomingMessage,
) -> Option<Result<CommandOutcome>> {
    let (name, args) = parse_command_line(text)?;

    deps.handlers.refresh().await;
    let manifests = deps.handlers.manifests().await;

    // Only async handlers become commands — sync handlers strand on the
    // async-only handler_jobs queue (F070; see module doc). `name` may be
    // the raw handler id OR a live `<command>` override name/alias
    // (derive_handler_commands advertises the override, so dispatch must
    // resolve it back to the handler id it enqueues under).
    let handler_id = resolve_command_to_handler_id(&manifests, &name)?;

    // Session resolution mirrors the builtin commands (`pipeline::commands`):
    // scoped by chat, not just user_id, so a group chat's /new doesn't act on
    // a different chat's session. No active session → let ordinary bootstrap
    // create one; this command simply doesn't fire this turn.
    let chat_scope = msg.chat_scope();
    let session_id = match crate::db::sessions::find_active_session(
        deps.db,
        deps.agent_name,
        &msg.user_id,
        &msg.channel,
        deps.dm_scope,
        chat_scope.as_deref(),
    )
    .await
    {
        Ok(Some(sid)) => sid,
        _ => return None,
    };

    let enabled = crate::agent::fse::get_enabled_allowlist(deps.db).await;

    // Mirror `derive_handler_commands`' advertising rule: a builtin-tier handler
    // outside the operator allowlist is NOT advertised as a command (absent from
    // `/help` and `/api/commands`), so a typed `/name` for it must fall through
    // to ordinary LLM processing — not resolve here and then reject with a
    // confusing "недоступен для этого источника".
    if manifests
        .iter()
        .find(|m| m.id == handler_id)
        .is_some_and(|m| m.tier == "builtin" && !enabled.iter().any(|e| e == &handler_id))
    {
        return None;
    }

    let lang = deps.agent_language;

    // ── Source resolution (F4) ──────────────────────────────────────────
    // 1) explicit arg (non-empty) → treated as a url/path source_ref.
    // 2) else, the most-recent client_upload attached to this session
    //    (the composer creates that row keyed to the message before
    //    dispatch, so this naturally covers a same-message web attachment).
    // 3) else → argsMenu asking for a url/file. Channel `msg.attachments[].url`
    //    handling is Phase 2b — NOT added here (MediaAttachment carries a URL,
    //    not an upload id, so it can't feed match_buttons' mime/size gate).
    let (upload_id, source_ref, gated_ok) = if !args.is_empty() {
        let ok = match_url_handlers(&manifests, &args, &enabled, lang)
            .iter()
            .any(|b| b.id == handler_id);
        (None, Some(args.clone()), ok)
    } else if let Some(uid) = resolve_recent_upload(deps, session_id).await {
        match crate::db::uploads::get_by_id(deps.db, uid).await.ok().flatten() {
            Some(row) => {
                let size = u64::try_from(row.size_bytes.max(0)).unwrap_or(0);
                let mut buttons = match_buttons(&manifests, &row.mime, size, &enabled, lang);
                retain_async_handlers(&mut buttons, &manifests);
                (Some(uid), None, buttons.iter().any(|b| b.id == handler_id))
            }
            None => (None, None, false),
        }
    } else {
        let card = serde_json::json!({
            "card_type": "command_args_menu",
            "command": name,
            "text": format!("Пришлите ссылку или файл для /{name}."),
        });
        return Some(Ok(CommandOutcome::Menu { card }));
    };

    // Trust gate (F6): the resolved handler MUST be in the matched
    // (domain/mime-filtered, allowlist-gated) set for the resolved source —
    // never enqueue a handler that wasn't offered for it.
    if !gated_ok {
        return Some(Ok(CommandOutcome::Text(format!(
            "Обработчик `{handler_id}` недоступен для этого источника."
        ))));
    }

    // Choice-valve gate (Task 2): if the resolved handler declares an
    // operator valve with `choices`, stash the run context and ask the user
    // to pick a value instead of enqueuing immediately. Task 3 adds the
    // endpoint that completes the stashed run on button click.
    if let Some(rm) = manifests.iter().find(|m| m.id == handler_id)
        && let Some((valve, choices)) = first_choice_valve(rm)
    {
        let mut stash = serde_json::json!({
            "kind": "command_choice",
            "handler_id": handler_id,
            "source_url": source_ref,
            "upload_id": upload_id.map(|u| u.to_string()),
            "session_id": session_id.to_string(),
            "agent": deps.agent_name,
            "valve": valve,
            "choices": choices,
            "language": lang,
        });
        // Bind the menu to the originating chat, mirroring the `hm:`
        // handler-menu stash (run.rs) — activates the existing `_chat_id`
        // origin-binding check in `command_menu_run`
        // (gateway/handlers/files.rs), blocking replay of a leaked `cm:`
        // token from another chat. Only inserted when a chat_id is actually
        // present (web/UI turns carry none) — the check is `if let Some(
        // stored_chat) = ctx.get("_chat_id")`, so a JSON `null` value would
        // wrongly arm it; `!is_null()` filters that out.
        if let Some(chat) = msg.context.get("chat_id").filter(|v| !v.is_null()).cloned()
            && let Some(obj) = stash.as_object_mut()
        {
            obj.insert("_chat_id".to_string(), chat);
        }
        let token = crate::gateway::handlers::files::store_menu_ctx(stash);
        let card = serde_json::json!({
            "card_type": "command_args_menu",
            "command": name,
            "text": format!("Выберите значение «{valve}» для /{name}:"),
            "options": choices.iter().map(|c| serde_json::json!({"value": c, "label": c})).collect::<Vec<_>>(),
            "token": token,
        });
        return Some(Ok(CommandOutcome::Menu { card }));
    }

    let params = serde_json::json!({ "language": lang });
    match opex_db::handler_jobs::insert_handler_job(
        deps.db,
        upload_id,
        source_ref.as_deref(),
        &handler_id,
        deps.agent_name,
        session_id,
        &params,
    )
    .await
    {
        Ok(_) => Some(Ok(CommandOutcome::Text(format!(
            "✅ Запустил `/{name}`. Результат придёт в чат по готовности."
        )))),
        Err(e) => Some(Ok(CommandOutcome::Text(format!(
            "Ошибка постановки задачи: {e}"
        )))),
    }
}

/// Most-recent `client_upload` row attached to a message in this session.
async fn resolve_recent_upload(deps: &HandlerDispatchDeps<'_>, session_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT u.id FROM uploads u \
         WHERE u.owner_type='client_upload' \
           AND u.owner_id IN (SELECT m.id::text FROM messages m WHERE m.session_id=$1) \
         ORDER BY u.created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(deps.db)
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::{first_choice_valve, parse_command_line, resolve_command_to_handler_id};
    use crate::agent::handler_registry::HandlerManifest;
    use serde_json::json;

    #[test]
    fn first_choice_valve_detected() {
        let m: HandlerManifest = serde_json::from_value(json!({
            "id": "summarize_video", "execution": "async", "tier": "workspace",
            "descriptions": {"en": "d"},
            "config": [{"name": "summary_length", "type": "string", "choices": ["short", "medium", "long"]}]
        }))
        .unwrap();
        let got = first_choice_valve(&m);
        assert_eq!(
            got,
            Some((
                "summary_length".to_string(),
                vec!["short".into(), "medium".into(), "long".into()]
            ))
        );
    }

    #[test]
    fn no_choice_valve_is_none() {
        let m: HandlerManifest = serde_json::from_value(json!({
            "id": "x", "execution": "async", "tier": "workspace",
            "descriptions": {"en": "d"},
            "config": [{"name": "lang", "type": "string"}]
        }))
        .unwrap();
        assert_eq!(first_choice_valve(&m), None);
    }

    #[test]
    fn parses_name_and_args() {
        assert_eq!(
            parse_command_line("/summarize_video https://x/y"),
            Some(("summarize_video".into(), "https://x/y".into()))
        );
        assert_eq!(parse_command_line("/transcribe"), Some(("transcribe".into(), "".into())));
        assert_eq!(parse_command_line("no slash"), None);
    }

    #[test]
    fn strips_botname_suffix_and_lowercases() {
        assert_eq!(
            parse_command_line("/Summarize_Video@my_bot  https://x/y  "),
            Some(("summarize_video".into(), "https://x/y".into()))
        );
    }

    #[test]
    fn bare_slash_is_not_a_command() {
        assert_eq!(parse_command_line("/"), None);
        assert_eq!(parse_command_line("/  "), None);
    }

    fn manifest_with_override(id: &str, exec: &str, command: Option<serde_json::Value>) -> HandlerManifest {
        let mut v = json!({
            "id": id, "execution": exec, "tier": "workspace",
            "descriptions": {"en": format!("{id} desc")}, "config": []
        });
        if let Some(c) = command {
            v["command"] = c;
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn resolves_by_handler_id() {
        let m = vec![manifest_with_override("summarize_video", "async", None)];
        assert_eq!(
            resolve_command_to_handler_id(&m, "summarize_video"),
            Some("summarize_video".to_string())
        );
    }

    #[test]
    fn resolves_by_override_name_and_valid_alias() {
        let m = vec![manifest_with_override(
            "summarize_video",
            "async",
            Some(json!({"name": "sumvid", "aliases": ["sv"]})),
        )];
        assert_eq!(
            resolve_command_to_handler_id(&m, "sumvid"),
            Some("summarize_video".to_string())
        );
        assert_eq!(
            resolve_command_to_handler_id(&m, "sv"),
            Some("summarize_video".to_string())
        );
        // The id itself is no longer the advertised name, but must still
        // resolve — dispatch stays a superset of what's advertised.
        assert_eq!(
            resolve_command_to_handler_id(&m, "summarize_video"),
            Some("summarize_video".to_string())
        );
    }

    #[test]
    fn unknown_name_and_sync_handler_return_none() {
        let m = vec![
            manifest_with_override("summarize_video", "async", None),
            manifest_with_override("describe", "sync", None),
        ];
        assert_eq!(resolve_command_to_handler_id(&m, "does_not_exist"), None);
        // A sync handler's own id must not resolve — sync handlers never
        // become dispatchable commands (F070).
        assert_eq!(resolve_command_to_handler_id(&m, "describe"), None);
    }

    #[test]
    fn invalid_alias_derive_dropped_is_not_dispatchable() {
        let m = vec![manifest_with_override(
            "summarize_video",
            "async",
            Some(json!({"name": "sumvid", "aliases": ["bad alias!"]})),
        )];
        assert_eq!(resolve_command_to_handler_id(&m, "bad alias!"), None);
    }

    #[test]
    fn resolves_case_insensitively_matching_lowercased_input() {
        // `parse_command_line` lowercases the typed name, so an uppercase-
        // containing override must still resolve against that lowercased form —
        // otherwise the advertised `/SumVid` is advertised-but-dead.
        let m = vec![manifest_with_override(
            "summarize_video",
            "async",
            Some(json!({"name": "SumVid", "aliases": ["SV"]})),
        )];
        assert_eq!(
            resolve_command_to_handler_id(&m, "sumvid"),
            Some("summarize_video".to_string())
        );
        assert_eq!(
            resolve_command_to_handler_id(&m, "sv"),
            Some("summarize_video".to_string())
        );
    }
}
