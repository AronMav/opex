//! `file_handler` system tool — the model-driven replacement for the per-adapter
//! inline action buttons.
//!
//! When the user sends a video/file link or an uploaded file, the engine injects
//! a context hint listing the matching handlers (see `pipeline::subagent`). The
//! model presents the options to the user and, once they choose, calls this tool
//! to run the selected handler. This works uniformly on every channel (Telegram,
//! Discord, Matrix, web, …) without any adapter-specific button code.
//!
//! Actions:
//! - `list` — return the handlers available for a `source_url` OR `upload_id`.
//! - `run`  — enqueue the chosen `handler_id` for that source. The requested
//!   handler MUST be in the matched (trust-gated, domain/mime-filtered) set, so
//!   the model cannot run a denied or mismatched handler.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::handler_registry::{match_buttons, match_url_handlers, HandlerButton, HandlerRegistry};
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct FileHandlerToolHandler;

#[async_trait]
impl SystemToolHandler for FileHandlerToolHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        handle_file_handler(deps, args).await
    }
}

async fn handle_file_handler(deps: ToolDeps<'_>, args: &Value) -> String {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    let source_url = args
        .get("source_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let upload_id_str = args
        .get("upload_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let lang = deps.cfg.agent.language.as_str();

    // Fresh registry handle (conditional GET with toolgate's ETag — cheap).
    let reg = HandlerRegistry::new(deps.toolgate_url.clone(), deps.http_client.clone());
    reg.refresh().await;
    let manifests = reg.manifests().await;
    let enabled = crate::agent::fse::get_enabled_allowlist(deps.db).await;

    // Resolve candidate handlers for the given source (url OR upload).
    let (buttons, upload_id): (Vec<HandlerButton>, Option<uuid::Uuid>) = if let Some(url) = source_url {
        (match_url_handlers(&manifests, url, &enabled, lang), None)
    } else if let Some(uid) = upload_id_str {
        let Ok(uuid) = uuid::Uuid::parse_str(uid) else {
            return "Error: upload_id is not a valid UUID".to_string();
        };
        let row = crate::db::uploads::get_by_id(deps.db, uuid).await.ok().flatten();
        let Some(row) = row else {
            return format!("Error: upload {uid} not found");
        };
        let size = u64::try_from(row.size_bytes).unwrap_or(0);
        // F070: the model-driven menu's `run` action enqueues onto the async-only
        // handler_jobs queue. Drop sync handlers here so they are neither offered
        // (list) nor enqueued+stranded (run); sync handlers run inline via the
        // composer's /api/files/{id}/run path. Mirrors match_url_handlers.
        let mut b = match_buttons(&manifests, &row.mime, size, &enabled, lang);
        crate::agent::handler_registry::retain_async_handlers(&mut b, &manifests);
        (b, Some(uuid))
    } else {
        return "Error: provide either source_url or upload_id".to_string();
    };

    match action {
        "list" => {
            if buttons.is_empty() {
                return "No handlers are available for this source.".to_string();
            }
            // Bullet list of handlers (id + label + description) — the raw material
            // for both the model context and text-only channels.
            let mut list_body = String::new();
            let mut items: Vec<Value> = Vec::new();
            for b in &buttons {
                let desc = manifests
                    .iter()
                    .find(|m| m.id == b.id)
                    .and_then(|m| m.descriptions.get(lang).or_else(|| m.descriptions.get("en")))
                    .cloned()
                    .unwrap_or_default();
                list_body.push_str(&format!("- {} ({})", b.id, b.label));
                if !desc.is_empty() {
                    list_body.push_str(&format!(" — {desc}"));
                }
                list_body.push('\n');
                items.push(json!({ "id": b.id, "label": b.label, "description": desc }));
            }

            // Emit a clickable menu card. Channels that render rich cards (web) or
            // inline buttons (Telegram) show the menu VISUALLY — so the model must
            // NOT re-list the handlers as text (that duplicates the menu). The
            // card's `text` field is what the model sees, so we phrase it as an
            // instruction, not user-facing content. Without a session there is no
            // card: fall back to a plain list the model presents itself.
            match deps.session_id {
                Some(session_id) => {
                    let instruction = format!(
                        "An interactive selection menu with these handlers has ALREADY been \
                         shown to the user (as clickable buttons):\n{list_body}\n\
                         This menu IS the complete response to the user. Reply with an EMPTY \
                         message — write NO text at all (not even 'ожидаю выбора' / 'waiting'): \
                         any text you add only clutters the chat and goes stale once the user \
                         clicks. Simply stop. When the user later picks one, call file_handler \
                         again with action=\"run\", the chosen handler_id, and the same \
                         source_url/upload_id."
                    );
                    let card = json!({
                        "card_type": "handler_menu",
                        "text": instruction,
                        "handlers": items,
                        "source_url": source_url,
                        "upload_id": upload_id.map(|u| u.to_string()),
                        "session_id": session_id.to_string(),
                        "agent": deps.agent_name,
                    });
                    format!("{}{}", crate::agent::engine::RICH_CARD_PREFIX, card)
                }
                None => format!(
                    "Available handlers:\n{list_body}\nTo run one, call file_handler again with \
                     action=\"run\", the chosen handler_id, and the same source_url/upload_id."
                ),
            }
        }
        "run" => {
            let handler_id = args.get("handler_id").and_then(Value::as_str).unwrap_or("");
            if handler_id.is_empty() {
                return "Error: handler_id is required for action=run".to_string();
            }
            // The requested handler MUST be in the matched (trust-gated,
            // domain/mime-filtered) set — the security boundary that the button
            // endpoints previously enforced.
            if !buttons.iter().any(|b| b.id == handler_id) {
                return format!(
                    "Error: handler '{handler_id}' is not available for this source (choose one from action=list)"
                );
            }
            let Some(session_id) = deps.session_id else {
                return "Error: no session to attach the job to".to_string();
            };
            let params = json!({ "language": lang });
            match opex_db::handler_jobs::insert_handler_job(
                deps.db,
                upload_id,
                source_url,
                handler_id,
                deps.agent_name,
                session_id,
                &params,
            )
            .await
            {
                Ok(_) => format!(
                    "✅ Started handler `{handler_id}`. The result will appear in the chat when it finishes."
                ),
                Err(e) => format!("Error: failed to enqueue handler job: {e}"),
            }
        }
        _ => "Error: action must be \"list\" or \"run\"".to_string(),
    }
}
