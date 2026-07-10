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
//! (describe/extract_document/save) run inline via `/api/files/{id}/run` and
//! would strand on this queue (no `/complete` callback is ever posted for
//! them).

use anyhow::Result;
use opex_types::IncomingMessage;
use uuid::Uuid;

use super::spec::CommandOutcome;
use crate::agent::handler_registry::{
    HandlerRegistry, match_buttons, match_url_handlers, retain_async_handlers,
};

/// Dependencies for handler-command dispatch, built by the engine wrapper
/// (`context_builder.rs::handle_command`) from `AgentEngine::cfg()`. Deliberately
/// does NOT carry a pre-resolved `session_id` — `try_handler_command` resolves
/// it itself via `find_active_session` (mirroring how the builtin commands in
/// `pipeline::commands` resolve theirs), so a message with no active session
/// falls through to ordinary (non-command) processing instead of erroring.
pub struct HandlerDispatchDeps<'a> {
    pub db: &'a sqlx::PgPool,
    pub toolgate_url: String,
    pub http: reqwest::Client,
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

    let reg = HandlerRegistry::new(deps.toolgate_url.clone(), deps.http.clone());
    reg.refresh().await;
    let manifests = reg.manifests().await;

    // Only async handlers become commands — sync handlers strand on the
    // async-only handler_jobs queue (F070; see module doc).
    if !manifests.iter().any(|m| m.execution == "async" && m.id == name) {
        return None;
    }

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
            .any(|b| b.id == name);
        (None, Some(args.clone()), ok)
    } else if let Some(uid) = resolve_recent_upload(deps, session_id).await {
        match crate::db::uploads::get_by_id(deps.db, uid).await.ok().flatten() {
            Some(row) => {
                let size = u64::try_from(row.size_bytes.max(0)).unwrap_or(0);
                let mut buttons = match_buttons(&manifests, &row.mime, size, &enabled, lang);
                retain_async_handlers(&mut buttons, &manifests);
                (Some(uid), None, buttons.iter().any(|b| b.id == name))
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
            "Обработчик `{name}` недоступен для этого источника."
        ))));
    }

    let params = serde_json::json!({ "language": lang });
    match opex_db::handler_jobs::insert_handler_job(
        deps.db,
        upload_id,
        source_ref.as_deref(),
        &name,
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
    use super::parse_command_line;

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
}
