//! Shared single-binding run helper used by both the HTTP deferred-run endpoint
//! (Task 6.8: POST /api/file-scenarios/run) and the Telegram `fse:<id>:<action>`
//! callback (Task 6.10).
//!
//! The original SSE stream is gone by the time this is called — the helper persists
//! a new assistant message row so the client can refetch it (spec §4.4a).

use anyhow::{anyhow, Result};
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::file_scenario::dispatch::{dispatch_action, DispatchInput};
use crate::agent::file_scenario::outcome::{ScenarioOutcome, ScenarioStatus};
use hydeclaw_types::{MediaAttachment, MediaType};

/// Per-execution ceiling matching the Phase-3 seam constant.
const BUILTIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Derive a coarse [`MediaType`] from a stored MIME string. Used to populate the
/// [`MediaAttachment`] the dispatcher expects.
fn media_type_from_mime(mime: &str) -> MediaType {
    let family = mime.split('/').next().unwrap_or("");
    match family {
        "image" => MediaType::Image,
        "audio" | "video" => MediaType::Audio,
        _ => MediaType::Document,
    }
}

/// Resolve an ENABLED binding, run it through the built-in dispatcher
/// (which re-applies `uploads_local_url` + per-execution timeout), then persist a
/// new assistant message row from the outcome.
///
/// Returns `(outcome, persisted_message_id)`. The original SSE stream is gone by
/// now — delivery is via the persisted row + a client `sessionMessages` invalidation
/// (spec §4.4a). Task 6.8 adds the HTTP handler; Task 6.10 adds the Telegram path.
#[allow(dead_code)] // Task 6.8: called by POST /api/file-scenarios/run
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_scenario_and_persist(
    db: &PgPool,
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    agent_language: &str,
    session_id: Uuid,
    upload_id: Uuid,
    scenario_id: Uuid,
) -> Result<(ScenarioOutcome, Uuid)> {
    // 1. Binding must exist AND be enabled.
    let binding = crate::db::file_scenarios::get_enabled_by_id(db, scenario_id)
        .await?
        .ok_or_else(|| anyhow!("scenario {scenario_id} not found or disabled"))?;

    // 2. Upload must exist (expiry enforced by get_by_id which hides expired rows).
    let upload = crate::db::uploads::get_by_id(db, upload_id)
        .await?
        .ok_or_else(|| anyhow!("upload {upload_id} not found or expired"))?;

    // 3. Build a MediaAttachment. The URL must contain `/api/uploads/{id}` so that
    //    `uploads_local_url` inside the dispatcher can rewrite it to localhost.
    //    We use `http://127.0.0.1:{port}/api/uploads/{id}` — the dispatcher will
    //    strip everything before `/api/uploads/` and substitute the listen address.
    let attachment_url = format!("http://127.0.0.1/api/uploads/{upload_id}");
    let attachment = MediaAttachment {
        url: attachment_url,
        media_type: media_type_from_mime(&upload.mime),
        file_name: None,
        mime_type: Some(upload.mime.clone()),
        file_size: Some(upload.size_bytes as u64),
    };

    // 4. Run via the Phase-2/3 single built-in dispatcher.
    //    `executor=skill` is not yet implemented in the deferred path; return
    //    Unsupported so the persisted message surfaces the reason to the user.
    let outcome = if binding.executor == "tool" {
        dispatch_action(DispatchInput {
            action_ref: &binding.action_ref,
            attachment: &attachment,
            toolgate_url,
            gateway_listen,
            language: agent_language,
            http_client,
            timeout: BUILTIN_TIMEOUT,
        })
        .await
    } else {
        ScenarioOutcome::unsupported(format!(
            "executor '{}' is not supported in the deferred-run path",
            binding.executor
        ))
    };

    // 5. Persist a new assistant message row from the outcome. The body carries
    //    `summary_text` and, for failure statuses, appends the reason. Artifact URLs
    //    are kept separate (stored in artifact_urls, not in the message body) so the
    //    frontend can reconstruct media parts.
    let mut body = outcome.summary_text.clone();
    if matches!(
        outcome.status,
        ScenarioStatus::Failed | ScenarioStatus::Unsupported | ScenarioStatus::TooLarge | ScenarioStatus::Timeout
    ) {
        if let Some(reason) = &outcome.reason {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&format!("(reason: {reason})"));
        }
    }

    let msg_id = hydeclaw_db::sessions::save_message_ex(
        db,
        session_id,
        "assistant",
        &body,
        None,
        None,
        None,
        None,
        None,
    )
    .await?;

    Ok((outcome, msg_id))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;
    use uuid::Uuid;

    #[sqlx::test(migrations = "../../migrations")]
    async fn run_disabled_binding_is_rejected(pool: PgPool) {
        // Seed a session + an upload + a DISABLED binding.
        let session_id =
            hydeclaw_db::sessions::create_new_session(&pool, "Hyde", "ui", "web")
                .await
                .unwrap();
        let upload_id = crate::db::uploads::insert_with_retention(
            &pool,
            "client_upload",
            None,
            "audio/ogg",
            b"OggS....",
            30,
        )
        .await
        .unwrap();
        let scenario_id = crate::db::file_scenarios::insert_for_test(
            &pool,
            "audio/*",
            "tool",
            "transcribe",
            "T",
            false,
            false, // enabled=false
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let res = run_scenario_and_persist(
            &pool,
            &http,
            "127.0.0.1:18789",
            "http://localhost:9011",
            "en",
            session_id,
            upload_id,
            scenario_id,
        )
        .await;
        assert!(res.is_err(), "disabled binding must be rejected");
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("not found or disabled"),
            "error must mention 'not found or disabled': {err}"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn run_unknown_scenario_id_is_rejected(pool: PgPool) {
        let session_id =
            hydeclaw_db::sessions::create_new_session(&pool, "Hyde", "ui", "web")
                .await
                .unwrap();
        let upload_id = crate::db::uploads::insert_with_retention(
            &pool,
            "client_upload",
            None,
            "audio/ogg",
            b"OggS",
            30,
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let res = run_scenario_and_persist(
            &pool,
            &http,
            "127.0.0.1:18789",
            "http://localhost:9011",
            "en",
            session_id,
            upload_id,
            Uuid::new_v4(), // unknown scenario_id
        )
        .await;
        assert!(res.is_err(), "unknown scenario_id must be rejected");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn run_enabled_save_binding_persists_assistant_message(pool: PgPool) {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Stand up a mock server to handle the localhost download inside dispatch_action.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"OggSfake".to_vec()),
            )
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");

        let session_id =
            hydeclaw_db::sessions::create_new_session(&pool, "Hyde", "ui", "web")
                .await
                .unwrap();
        let upload_id = crate::db::uploads::insert_with_retention(
            &pool,
            "client_upload",
            None,
            "audio/ogg",
            b"OggSfake",
            30,
        )
        .await
        .unwrap();
        let scenario_id = crate::db::file_scenarios::insert_for_test(
            &pool,
            "audio/*",
            "tool",
            "save",
            "Save",
            false,
            true, // enabled=true
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let res = run_scenario_and_persist(
            &pool,
            &http,
            &gateway_listen,
            &server.uri(),
            "en",
            session_id,
            upload_id,
            scenario_id,
        )
        .await;

        let (outcome, msg_id) = res.expect("enabled save binding must succeed");
        assert_eq!(outcome.status, ScenarioStatus::Ok, "save always returns Ok");

        // Verify the assistant message was persisted to the DB.
        let messages = hydeclaw_db::sessions::load_messages(&pool, session_id, None)
            .await
            .unwrap();
        assert!(
            messages.iter().any(|m| m.id == msg_id && m.role == "assistant"),
            "persisted message must appear in session messages"
        );
    }
}
