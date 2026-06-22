//! Per-attachment dispatch seam: download → sniff → lookup bindings → branch → run.
//!
//! This is the integration heart of Phase 3. For each inbound attachment it:
//! 1. Downloads the full upload body once via localhost (no Range support in uploads_serve).
//! 2. Sniffs the effective MIME type from the first 8 KB.
//! 3. Looks up enabled bindings for the sniffed type (glob matching).
//! 4. Branches: 0 bindings → `save`; ≥1 with a default → run it + record alternatives;
//!    ≥2 without a default → `save` + offer all as alternatives (Phase 6 emits them).

use crate::agent::file_scenario::dispatch::{dispatch_action, DispatchInput};
use crate::agent::file_scenario::outcome::ScenarioOutcome;
use crate::agent::file_scenario::sniff::sniff_bytes;
use crate::agent::fse::{get_enabled_allowlist, is_allowed_for_autorun};
use crate::agent::url_tools::uploads_local_url;
use crate::db::audit::event_types::FSE_AUTO_RUN;
use hydeclaw_types::MediaAttachment;
use uuid::Uuid;

/// How long a single built-in execution may run before it is aborted.
const BUILTIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Number of prefix bytes downloaded for sniffing (server does not support Range).
const SNIFF_PREFIX_BYTES: usize = 8 * 1024;

// ── Public types (consumed by Task 3.6) ─────────────────────────────────────

/// A non-default binding that was NOT run but is available as a user-selectable
/// alternative. Emitted to the UI in Phase 6; recorded here for completeness.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScenarioChoice {
    pub scenario_id: Uuid,
    pub label: String,
    pub executor: String,
}

/// Post-hoc alternatives produced for one attachment when ≥1 non-default
/// enabled binding exists. Emission as UI chips is Phase 6's job.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingAlternative {
    pub upload_id: Uuid,
    pub match_type: String,
    pub alternatives: Vec<ScenarioChoice>,
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Download the full body of an upload from localhost (the `/api/uploads` handler
/// does NOT honor HTTP Range — verified in uploads_serve.rs). Returns `None` on
/// any download error (treated as a failed outcome by the caller).
async fn download_full(
    http_client: &reqwest::Client,
    gateway_listen: &str,
    url: &str,
) -> Option<bytes::Bytes> {
    let local = uploads_local_url(url, gateway_listen);
    match http_client.get(&local).send().await {
        Ok(resp) if resp.status().is_success() => resp.bytes().await.ok(),
        Ok(resp) => {
            tracing::warn!(
                url = %local,
                status = %resp.status(),
                "fse: download for sniff failed"
            );
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, url = %local, "fse: download for sniff errored");
            None
        }
    }
}

/// Extract the upload UUID from a signed URL like
/// `/api/uploads/{uuid}?sig=…&exp=…`. Returns `None` for malformed URLs.
pub fn upload_id_from_url(url: &str) -> Option<Uuid> {
    // Find the UUID segment between /api/uploads/ and the next ?
    let after = url.find("/api/uploads/").map(|i| &url[i + "/api/uploads/".len()..])?;
    let uuid_str = after.split(['?', '/']).next()?;
    Uuid::parse_str(uuid_str).ok()
}

/// Run the named built-in action against an attachment, using the already-downloaded
/// bytes buffer so no second download is needed. Wraps `dispatch_action` with the
/// standard per-execution timeout.
async fn run_builtin(
    action_ref: &str,
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    agent_language: &str,
    attachment: &MediaAttachment,
) -> ScenarioOutcome {
    dispatch_action(DispatchInput {
        action_ref,
        attachment,
        toolgate_url,
        gateway_listen,
        language: agent_language,
        http_client,
        timeout: BUILTIN_TIMEOUT,
    })
    .await
}

// ── Main seam ────────────────────────────────────────────────────────────────

/// Run the deterministic FSE dispatch for every inbound attachment.
///
/// For each attachment:
/// - Downloads the full body once (no Range support).
/// - Sniffs the effective MIME from the first [`SNIFF_PREFIX_BYTES`].
/// - Queries enabled bindings that glob-match the sniffed MIME.
/// - Branches:
///   - **0 bindings** → run `save` (universal rowless fallback).
///   - **≥1 with a default** (`is_default = true`, `executor = tool`) → run the
///     highest-priority default; record non-default bindings as `pending_alternatives`.
///   - **≥1 without any default** → run `save` immediately; record all bindings as
///     `pending_alternatives` (Phase 6 emits them as UI chips).
///
/// Returns `(outcomes, pending_alternatives)`. One outcome per attachment.
/// `pending_alternatives` are produced but NOT emitted as SSE events here (Phase 6).
pub async fn dispatch_attachments(
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    agent_language: &str,
    db: &sqlx::PgPool,
    _enriched: &mut String,
    attachments: &[MediaAttachment],
) -> (Vec<ScenarioOutcome>, Vec<PendingAlternative>) {
    let mut outcomes = Vec::with_capacity(attachments.len());
    let mut pending: Vec<PendingAlternative> = Vec::new();

    // Fetch the operator allowlist once per request — it is a per-request
    // invariant (a system_flags row), not per-attachment. Hoisting avoids
    // N redundant DB reads for N attachments in a single call.
    let enabled_allowlist = get_enabled_allowlist(db).await;

    for att in attachments {
        // 1. Download full bytes once.
        let bytes = download_full(http_client, gateway_listen, &att.url).await;
        let bytes = match bytes {
            Some(b) => b,
            None => {
                outcomes.push(ScenarioOutcome::failed("download for sniff failed".into()));
                continue;
            }
        };

        // 2. Sniff effective MIME from the bounded prefix.
        let prefix = &bytes[..bytes.len().min(SNIFF_PREFIX_BYTES)];
        let sniffed = sniff_bytes(
            prefix,
            att.file_name.as_deref(),
            att.mime_type.as_deref(),
            att.media_type.clone(),
        );

        // 3. Look up enabled bindings for the sniffed MIME (glob-matched, priority ASC).
        let bindings =
            crate::db::file_scenarios::list_enabled_for_match_type(db, &sniffed.mime)
                .await
                .unwrap_or_default();

        // 4. Branch 0/1/2+.
        let default_binding = bindings.iter().find(|b| b.is_default && b.executor == "tool");

        match default_binding {
            Some(b) => {
                // ≥1 bindings and a default exists → re-check the operator allowlist
                // (defense-in-depth per design §4.6) before auto-running.
                // `enabled_allowlist` was fetched once before the loop.
                let action_to_run = if is_allowed_for_autorun(&b.action_ref, &enabled_allowlist) {
                    b.action_ref.as_str()
                } else {
                    // Action disabled by operator or not in the constant (forged row) →
                    // fail-close to `save`, same as the 0-bindings / no-default branch.
                    tracing::warn!(
                        action_ref = %b.action_ref,
                        "fse: default binding blocked by allowlist re-check; falling back to save"
                    );
                    "save"
                };
                let outcome = run_builtin(
                    action_to_run,
                    http_client,
                    gateway_listen,
                    toolgate_url,
                    agent_language,
                    att,
                )
                .await;

                // I-3: Audit the auto-run only when the configured default action ran —
                // NOT when the allowlist blocked it and we fell back to `save`.
                // `action_to_run == b.action_ref` means the binding's own action ran;
                // a fallback overwrites action_to_run with "save" which != action_ref
                // (unless the binding itself is action_ref="save", which is a valid
                // configured auto-run and SHOULD be audited).
                // Discriminate by tracking whether the allowlist check passed:
                if is_allowed_for_autorun(&b.action_ref, &enabled_allowlist) {
                    let upload_ref = upload_id_from_url(&att.url)
                        .map(|id| id.to_string())
                        .unwrap_or_default();
                    crate::db::audit::audit_spawn(
                        db.clone(),
                        String::new(),
                        FSE_AUTO_RUN,
                        Some("system".into()),
                        serde_json::json!({
                            "scenario_id": b.id.to_string(),
                            "match_type": b.match_type,
                            "action_ref": action_to_run,
                            "upload_ref": upload_ref,
                        }),
                    );
                }

                outcomes.push(outcome);

                // Non-default bindings → post-hoc alternatives (Phase 6 emits).
                let alts: Vec<ScenarioChoice> = bindings
                    .iter()
                    .filter(|x| !x.is_default)
                    .map(|x| ScenarioChoice {
                        scenario_id: x.id,
                        label: x.label.clone(),
                        executor: x.executor.clone(),
                    })
                    .collect();
                if !alts.is_empty()
                    && let Some(upload_id) = upload_id_from_url(&att.url)
                {
                    pending.push(PendingAlternative {
                        upload_id,
                        match_type: sniffed.mime.clone(),
                        alternatives: alts,
                    });
                }
            }
            None if bindings.is_empty() => {
                // 0 bindings → save (deterministic universal fallback).
                let outcome = run_builtin(
                    "save",
                    http_client,
                    gateway_listen,
                    toolgate_url,
                    agent_language,
                    att,
                )
                .await;
                outcomes.push(outcome);
            }
            None => {
                // ≥1 bindings but no default (no blocking primitive in web) → save
                // immediately and offer all as post-hoc alternatives.
                let outcome = run_builtin(
                    "save",
                    http_client,
                    gateway_listen,
                    toolgate_url,
                    agent_language,
                    att,
                )
                .await;
                outcomes.push(outcome);

                let alts: Vec<ScenarioChoice> = bindings
                    .iter()
                    .map(|x| ScenarioChoice {
                        scenario_id: x.id,
                        label: x.label.clone(),
                        executor: x.executor.clone(),
                    })
                    .collect();
                if let Some(upload_id) = upload_id_from_url(&att.url) {
                    pending.push(PendingAlternative {
                        upload_id,
                        match_type: sniffed.mime.clone(),
                        alternatives: alts,
                    });
                }
            }
        }
    }

    (outcomes, pending)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hydeclaw_types::MediaType;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn att(url: &str, mt: MediaType) -> MediaAttachment {
        MediaAttachment {
            url: url.into(),
            media_type: mt,
            file_name: Some("x.bin".into()),
            mime_type: None,
            file_size: None,
        }
    }

    // ── upload_id_from_url ────────────────────────────────────────────────────

    #[test]
    fn upload_id_from_signed_url() {
        let id = Uuid::new_v4();
        let url = format!("https://h/api/uploads/{id}?sig=abc&exp=99");
        assert_eq!(upload_id_from_url(&url), Some(id));
    }

    #[test]
    fn upload_id_from_url_missing_path_is_none() {
        assert_eq!(upload_id_from_url("https://h/other/path"), None);
    }

    #[test]
    fn upload_id_from_url_non_uuid_segment_is_none() {
        assert_eq!(upload_id_from_url("https://h/api/uploads/not-a-uuid?sig=x"), None);
    }

    // ── 0-binding branch resolves to save ────────────────────────────────────

    /// 0-binding branch resolves to `save`: one Ok outcome, no chips.
    #[sqlx::test(migrations = "../../migrations")]
    async fn zero_binding_resolves_to_save(pool: sqlx::PgPool) {
        // Stand up a localhost "uploads" server returning arbitrary bytes so
        // uploads_local_url(...) → http://localhost:{port}/api/uploads/.. resolves.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"\x00\x01\x02\x03".to_vec()),
            )
            .mount(&server)
            .await;
        // gateway_listen carries the mock server's port so uploads_local_url targets it.
        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000001?sig=x&exp=1",
            server.uri()
        );

        let client = reqwest::Client::new();
        let mut enriched =
            format!("[User attached a document \"x.bin\": {upload_url}]");
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            "http://localhost:9011",
            "ru",
            &pool,
            &mut enriched,
            &[att(&upload_url, MediaType::Document)],
        )
        .await;
        assert_eq!(outcomes.len(), 1, "one outcome per attachment");
        assert!(pending.is_empty(), "0-binding save offers no alternatives");
    }

    // ── 1-default-binding branch runs that built-in ───────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn one_default_binding_runs_builtin(pool: sqlx::PgPool) {
        use crate::db::file_scenarios::create;

        // Insert a default `save` binding for `application/octet-stream` (the
        // sniffed type for 4 arbitrary bytes with no magic signature).
        create(
            &pool,
            "application/octet-stream",
            "tool",
            "save",
            "Save file",
            true,  // is_default
            50,
            true,  // enabled
            "test",
        )
        .await
        .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"\x00\x01\x02\x03".to_vec()),
            )
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000002?sig=x&exp=1",
            server.uri()
        );

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            "http://localhost:9011",
            "ru",
            &pool,
            &mut enriched,
            &[att(&upload_url, MediaType::Document)],
        )
        .await;

        assert_eq!(outcomes.len(), 1, "one outcome");
        // The `save` built-in always returns Ok.
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "save returns Ok"
        );
        assert!(pending.is_empty(), "no non-default alternatives");
    }

    // ── 2+ bindings, no default → save + alternatives ─────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn two_bindings_no_default_save_with_alternatives(pool: sqlx::PgPool) {
        use crate::db::file_scenarios::create;

        // Two non-default bindings for `application/octet-stream`.
        let id_a = create(
            &pool,
            "application/octet-stream",
            "tool",
            "save",
            "Save",
            false, // not default
            50,
            true,
            "test",
        )
        .await
        .unwrap();

        // `extract_document` is in the allowlist; second binding.
        let id_b = create(
            &pool,
            "application/*",
            "tool",
            "extract_document",
            "Extract",
            false, // not default
            100,
            true,
            "test",
        )
        .await
        .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"\x00\x01\x02\x03".to_vec()),
            )
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000003?sig=x&exp=1",
            server.uri()
        );

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            "http://localhost:9011",
            "ru",
            &pool,
            &mut enriched,
            &[att(&upload_url, MediaType::Document)],
        )
        .await;

        assert_eq!(outcomes.len(), 1, "one outcome");
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "fallback save returns Ok"
        );
        assert_eq!(pending.len(), 1, "one pending alternative set");
        assert_eq!(pending[0].alternatives.len(), 2, "both bindings offered as alternatives");

        // Both scenario IDs should appear.
        let ids: Vec<Uuid> = pending[0].alternatives.iter().map(|a| a.scenario_id).collect();
        assert!(ids.contains(&id_a), "first binding in alternatives");
        assert!(ids.contains(&id_b), "second binding in alternatives");
    }

    // ── glob matching: image/* matches image/png ──────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn glob_image_star_matches_png(pool: sqlx::PgPool) {
        use crate::db::file_scenarios::create;

        // Insert a default binding for `image/*` (should match PNG bytes).
        create(
            &pool,
            "image/*",
            "tool",
            "save",
            "Save image",
            true,
            50,
            true,
            "test",
        )
        .await
        .unwrap();

        // PNG magic bytes: 8-byte signature.
        let png_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(png_bytes))
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000004?sig=x&exp=1",
            server.uri()
        );

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            "http://localhost:9011",
            "ru",
            &pool,
            &mut enriched,
            &[att(&upload_url, MediaType::Image)],
        )
        .await;

        assert_eq!(outcomes.len(), 1);
        // The `image/*` binding matched → ran the `save` default built-in.
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok
        );
        assert!(pending.is_empty(), "no non-default alternatives");
    }

    // ── seeded audio/* → transcribe (Link B: seam routes to built-in) ───────────

    /// Closes the coverage gap identified in Task 9.4 review (Link B):
    /// proves `dispatch_attachments` performs the DB binding-lookup from
    /// `seed_default_file_scenarios` AND routes `audio/ogg` through to the
    /// `transcribe` built-in end-to-end, via the seam's default-binding
    /// predicate (`is_default && executor == "tool"`).
    ///
    /// If the seam's binding-lookup or glob-match for `audio/*` were broken,
    /// this test would fall into the 0-binding (`save`) branch and the
    /// `summary_text` assertion would fail — catching the regression.
    #[sqlx::test(migrations = "../../migrations")]
    async fn seeded_audio_ogg_routes_to_transcribe(pool: sqlx::PgPool) {
        // 1. Seed the real default bindings (audio/*→transcribe, image/*→describe,
        //    application/pdf→extract_document) exactly as startup does.
        crate::agent::fse::seed_default_file_scenarios(&pool)
            .await
            .expect("seed must not fail");

        // 2. Wiremock doubles as upload host + toolgate.
        //    - GET /api/uploads/* → OGG magic bytes (sniff + transcribe download)
        //    - POST /transcribe   → {"text":"seam ok"}
        let server = MockServer::start().await;
        let ogg_bytes: Vec<u8> = b"OggSfakeaudiobytes".to_vec();
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ogg_bytes))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/transcribe"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"text": "seam ok"})),
            )
            .mount(&server)
            .await;

        // 3. gateway_listen → mock port so uploads_local_url rewrites correctly.
        let port = server.address().port();
        let gateway_listen = format!("0.0.0.0:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000006?sig=x&exp=1",
            server.uri()
        );

        let audio_att = MediaAttachment {
            url: upload_url.clone(),
            media_type: MediaType::Audio,
            file_name: Some("voice.ogg".into()),
            mime_type: Some("audio/ogg".into()),
            file_size: None,
        };

        // 4. Call the seam — the seam must look up the seeded `audio/*→transcribe`
        //    default binding, find it, and route to the `transcribe` built-in.
        let client = reqwest::Client::new();
        let mut enriched = format!("[User sent a voice message: {upload_url}]");
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            &server.uri(),
            "ru",
            &pool,
            &mut enriched,
            &[audio_att],
        )
        .await;

        // 5. Assertions: seeded audio/* → transcribe must route and succeed.
        assert_eq!(outcomes.len(), 1, "one outcome per attachment");
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "seeded audio/*→transcribe must return Ok; reason: {:?}",
            outcomes[0].reason
        );
        assert!(
            outcomes[0].summary_text.contains("seam ok"),
            "transcript 'seam ok' must appear in summary_text: {:?}",
            outcomes[0].summary_text
        );
        assert!(pending.is_empty(), "no non-default alternatives for seeded defaults");
    }

    // ── seeded image/* → describe (Link B: seam routes to built-in) ──────────

    /// Mirrors `seeded_audio_ogg_routes_to_transcribe` for the image path:
    /// proves the seam's DB binding-lookup finds the `image/*→describe` seeded
    /// default and routes JPEG bytes through to the `describe` built-in.
    ///
    /// If the seam's glob-match for `image/*` were broken (e.g. the predicate
    /// in `dispatch_seam.rs` line ~159 changed), this would fall to `save` and
    /// the `summary_text` assertion on "<vision>" would fail.
    #[sqlx::test(migrations = "../../migrations")]
    async fn seeded_image_jpeg_routes_to_describe(pool: sqlx::PgPool) {
        // 1. Seed the real default bindings.
        crate::agent::fse::seed_default_file_scenarios(&pool)
            .await
            .expect("seed must not fail");

        // 2. Wiremock doubles as upload host + toolgate.
        //    - GET /api/uploads/* → JPEG magic bytes (sniff + describe download)
        //    - POST /describe     → {"description":"seam ok image"}
        let server = MockServer::start().await;
        // JPEG SOI marker bytes: FF D8 FF
        let jpeg_bytes: Vec<u8> = b"\xFF\xD8\xFFfakeimagebytes".to_vec();
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg_bytes))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/describe"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"description": "seam ok image"})),
            )
            .mount(&server)
            .await;

        // 3. gateway_listen → mock port.
        let port = server.address().port();
        let gateway_listen = format!("0.0.0.0:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000007?sig=x&exp=1",
            server.uri()
        );

        let image_att = MediaAttachment {
            url: upload_url.clone(),
            media_type: MediaType::Image,
            file_name: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            file_size: None,
        };

        // 4. Call the seam.
        let client = reqwest::Client::new();
        let mut enriched = format!("[User attached an image: {upload_url}]");
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            &server.uri(),
            "ru",
            &pool,
            &mut enriched,
            &[image_att],
        )
        .await;

        // 5. Assertions: seeded image/*→describe must route and succeed.
        assert_eq!(outcomes.len(), 1, "one outcome per attachment");
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "seeded image/*→describe must return Ok; reason: {:?}",
            outcomes[0].reason
        );
        assert!(
            outcomes[0].summary_text.contains("seam ok image"),
            "description 'seam ok image' must appear in summary_text: {:?}",
            outcomes[0].summary_text
        );
        assert!(
            outcomes[0].summary_text.contains("<vision>"),
            "describe outcome must wrap description in <vision> tags: {:?}",
            outcomes[0].summary_text
        );
        assert!(pending.is_empty(), "no non-default alternatives for seeded defaults");
    }

    // ── seeded application/pdf → extract_document (Link B: document path) ───────

    /// Mirrors `seeded_audio_ogg_routes_to_transcribe` for the document path:
    /// proves the seam's DB binding-lookup finds the `application/pdf→extract_document`
    /// seeded default and routes PDF bytes through to the `extract_document` built-in.
    ///
    /// `extract_document` POSTs the localhost-rewritten URL (not the file bytes) to
    /// toolgate `POST /extract-text-url` with `{"document_url": ..., "max_chars": 8000}`.
    /// The mock returns `{"text": "seam pdf ok"}` which must appear in `summary_text`.
    ///
    /// If the seam's glob-match for `application/pdf` were broken, or the seeded
    /// row were missing, the 0-binding branch would fire (`save`), the
    /// `/extract-text-url` mock would receive no request, and the `summary_text`
    /// assertion would fail — catching the regression.
    #[sqlx::test(migrations = "../../migrations")]
    async fn seeded_pdf_routes_to_extract_document(pool: sqlx::PgPool) {
        // 1. Seed the real default bindings (audio/*→transcribe, image/*→describe,
        //    application/pdf→extract_document) exactly as startup does.
        crate::agent::fse::seed_default_file_scenarios(&pool)
            .await
            .expect("seed must not fail");

        // 2. Wiremock doubles as upload host + toolgate.
        //    - GET /api/uploads/* → PDF magic bytes (sniff confirms application/pdf)
        //    - POST /extract-text-url → {"text":"seam pdf ok"}
        //
        //    Note: unlike transcribe/describe, extract_document does NOT download the
        //    bytes itself — it sends the localhost-rewritten URL to toolgate and lets
        //    toolgate do the download. So the GET mock is only needed so `download_full`
        //    in the seam (Step 1) succeeds; the built-in itself only calls POST.
        let server = MockServer::start().await;
        // PDF magic bytes: "%PDF-1.4" — `infer` crate sniffs this as application/pdf.
        let pdf_bytes: Vec<u8> = b"%PDF-1.4 fake pdf bytes".to_vec();
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(pdf_bytes))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/extract-text-url"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"text": "seam pdf ok"})),
            )
            .mount(&server)
            .await;

        // 3. gateway_listen → mock port so uploads_local_url rewrites correctly.
        let port = server.address().port();
        let gateway_listen = format!("0.0.0.0:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000008?sig=x&exp=1",
            server.uri()
        );

        let pdf_att = MediaAttachment {
            url: upload_url.clone(),
            media_type: MediaType::Document,
            file_name: Some("report.pdf".into()),
            mime_type: Some("application/pdf".into()),
            file_size: None,
        };

        // 4. Call the seam — the seam must look up the seeded `application/pdf→extract_document`
        //    default binding, find it, and route to the `extract_document` built-in.
        let client = reqwest::Client::new();
        let mut enriched = format!("[User attached a document \"report.pdf\": {upload_url}]");
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            &server.uri(),
            "en",
            &pool,
            &mut enriched,
            &[pdf_att],
        )
        .await;

        // 5. Assertions: seeded application/pdf → extract_document must route and succeed.
        assert_eq!(outcomes.len(), 1, "one outcome per attachment");
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "seeded application/pdf→extract_document must return Ok; reason: {:?}",
            outcomes[0].reason
        );
        assert!(
            outcomes[0].summary_text.contains("seam pdf ok"),
            "extracted text 'seam pdf ok' must appear in summary_text: {:?}",
            outcomes[0].summary_text
        );
        assert!(pending.is_empty(), "no non-default alternatives for seeded defaults");
    }

    // ── Task 9.6: web 2+-no-default → immediate save + alternatives offered ─────

    /// On the web (sink-less) path, when ≥2 enabled bindings match but NONE is
    /// marked `is_default`, the dispatcher MUST NOT stall or block. It must:
    ///   1. Run `save` immediately (outcome status == Ok).
    ///   2. Record ALL matched bindings as `pending_alternatives` for Phase 6
    ///      to emit as UI chips — NOT leave the list empty.
    ///
    /// This is the **web affordance** contract: save prevents loss of the file
    /// while the user's explicit choice is pending. The pending_alternatives
    /// carry the available options so the UI can surface them without a round-trip.
    ///
    /// Brief (Task 9.6) incorrectly asserted `pending_alternatives.is_empty()`.
    /// The real design (dispatch_seam.rs, `None =>` branch, lines 208-236) and
    /// the existing `two_bindings_no_default_save_with_alternatives` unit test
    /// (line 388) both confirm that alternatives ARE populated. The test below
    /// guards the correct behavior.
    #[sqlx::test(migrations = "../../migrations")]
    async fn web_two_bindings_no_default_immediate_save_with_alternatives(pool: sqlx::PgPool) {
        use crate::db::file_scenarios::create;

        // Two non-default bindings for image/png (via image/* glob).
        // Both `save` and `describe` are in the FSE_DEFAULT_ALLOWLIST.
        let id_a = create(
            &pool,
            "image/*",
            "tool",
            "save",
            "Save image",
            false, // is_default = false
            50,
            true, // enabled
            "test",
        )
        .await
        .unwrap();

        let id_b = create(
            &pool,
            "image/*",
            "tool",
            "describe",
            "Describe image",
            false, // is_default = false
            100,
            true, // enabled
            "test",
        )
        .await
        .unwrap();

        // Serve PNG magic bytes so download_full succeeds and sniff recognises image/png.
        let png_bytes: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(png_bytes))
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");
        // Use a valid UUID in the upload URL so upload_id_from_url can extract it
        // and populate PendingAlternative.upload_id (matches production path).
        let upload_uuid = uuid::Uuid::new_v4();
        let upload_url = format!(
            "{}/api/uploads/{upload_uuid}?sig=x&exp=1",
            server.uri()
        );

        let client = reqwest::Client::new();
        let mut enriched = format!("[User attached an image: {upload_url}]");
        let (outcomes, pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            "http://localhost:9011", // toolgate not called by `save`
            "en",
            &pool,
            &mut enriched,
            &[att(&upload_url, MediaType::Image)],
        )
        .await;

        // 1. One outcome: save ran immediately (no stall, no block).
        assert_eq!(outcomes.len(), 1, "one outcome per attachment");
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "fallback save must return Ok immediately; reason: {:?}",
            outcomes[0].reason
        );

        // 2. The artifact URL must contain the upload path (save preserves the file).
        assert!(
            outcomes[0].artifact_urls.iter().any(|u| u.contains("/api/uploads/")),
            "save must record the upload URL in artifact_urls: {:?}",
            outcomes[0].artifact_urls
        );

        // 3. Alternatives ARE populated (not empty) — Phase 6 uses them to emit chips.
        assert_eq!(
            pending.len(),
            1,
            "pending_alternatives must contain one set (one attachment)"
        );
        assert_eq!(
            pending[0].alternatives.len(),
            2,
            "both non-default bindings must appear as alternatives"
        );
        let ids: Vec<uuid::Uuid> = pending[0].alternatives.iter().map(|a| a.scenario_id).collect();
        assert!(ids.contains(&id_a), "binding A must be in alternatives");
        assert!(ids.contains(&id_b), "binding B must be in alternatives");
        assert_eq!(
            pending[0].upload_id, upload_uuid,
            "PendingAlternative.upload_id must match the upload UUID from the URL"
        );
    }

    // ── Task 9.9b: allowlist re-check in the auto-run hot path ───────────────

    /// Test 1: Legitimate seeded default still runs when allowlist is all-enabled
    /// (the default, unset state). Proves no regression for the happy path.
    /// image/jpeg → seeded image/*→describe default → describe runs (Ok + <vision>).
    #[sqlx::test(migrations = "../../migrations")]
    async fn allowlist_enabled_default_still_runs(pool: sqlx::PgPool) {
        // Seed real default bindings; allowlist flag is unset → all entries enabled.
        crate::agent::fse::seed_default_file_scenarios(&pool)
            .await
            .expect("seed must not fail");

        let server = MockServer::start().await;
        let jpeg_bytes: Vec<u8> = b"\xFF\xD8\xFFfakejpeg".to_vec();
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg_bytes))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/describe"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"description": "allowlist ok"})),
            )
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("0.0.0.0:{port}");
        let upload_url = format!(
            "{}/api/uploads/10000000-0000-0000-0000-000000000001?sig=x&exp=1",
            server.uri()
        );
        let image_att = MediaAttachment {
            url: upload_url.clone(),
            media_type: MediaType::Image,
            file_name: Some("photo.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            file_size: None,
        };

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, _pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            &server.uri(),
            "en",
            &pool,
            &mut enriched,
            &[image_att],
        )
        .await;

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "describe must run when allowlist is fully enabled; reason: {:?}",
            outcomes[0].reason
        );
        assert!(
            outcomes[0].summary_text.contains("allowlist ok"),
            "describe result 'allowlist ok' must be in summary_text: {:?}",
            outcomes[0].summary_text
        );
        assert!(
            outcomes[0].summary_text.contains("<vision>"),
            "describe must wrap in <vision> tags: {:?}",
            outcomes[0].summary_text
        );
    }

    /// Test 2: Operator-disabled allowlist entry blocks auto-run and falls back to `save`.
    /// Seeds defaults, disables `describe` via `set_enabled_allowlist`, dispatches
    /// image/jpeg → must NOT describe; must resolve to save (Ok + no vision output).
    #[sqlx::test(migrations = "../../migrations")]
    async fn allowlist_disabled_entry_falls_back_to_save(pool: sqlx::PgPool) {
        use crate::agent::fse::set_enabled_allowlist;

        crate::agent::fse::seed_default_file_scenarios(&pool)
            .await
            .expect("seed must not fail");

        // Disable `describe` — operator removes it from the enabled set.
        set_enabled_allowlist(
            &pool,
            &["transcribe".to_string(), "extract_document".to_string(), "save".to_string()],
        )
        .await
        .expect("set_enabled_allowlist must not fail for valid subset");

        let server = MockServer::start().await;
        let jpeg_bytes: Vec<u8> = b"\xFF\xD8\xFFfakejpeg".to_vec();
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg_bytes))
            .mount(&server)
            .await;
        // /describe must NOT be called; if it is, this mock returns an unexpected response
        // and the describe-content assertion would fail.
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/describe"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"description": "SHOULD NOT APPEAR"})),
            )
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("0.0.0.0:{port}");
        let upload_url = format!(
            "{}/api/uploads/10000000-0000-0000-0000-000000000002?sig=x&exp=1",
            server.uri()
        );
        let image_att = MediaAttachment {
            url: upload_url.clone(),
            media_type: MediaType::Image,
            file_name: Some("blocked.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            file_size: None,
        };

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, _pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            &server.uri(),
            "en",
            &pool,
            &mut enriched,
            &[image_att],
        )
        .await;

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "save fallback must return Ok; reason: {:?}",
            outcomes[0].reason
        );
        // `save` does NOT produce vision output; must not contain describe result.
        assert!(
            !outcomes[0].summary_text.contains("SHOULD NOT APPEAR"),
            "describe must NOT run when operator disabled it: {:?}",
            outcomes[0].summary_text
        );
        assert!(
            !outcomes[0].summary_text.contains("<vision>"),
            "save outcome must not contain <vision> tags: {:?}",
            outcomes[0].summary_text
        );
        // `save` produces the save message.
        assert!(
            outcomes[0].summary_text.contains("saved"),
            "save fallback must mention 'saved': {:?}",
            outcomes[0].summary_text
        );
    }

    /// Test 3: Forged non-allowlisted default blocked by allowlist re-check (defense-in-depth).
    /// Directly inserts `is_default=true, executor="tool", action_ref="code_exec"` for image/*.
    /// Dispatching image/jpeg must NOT run code_exec; must resolve to save.
    /// This proves the seam's re-check fires even for a forged DB row,
    /// independently of `resolve()`'s Unsupported fallthrough.
    #[sqlx::test(migrations = "../../migrations")]
    async fn forged_non_allowlisted_default_blocked(pool: sqlx::PgPool) {
        use crate::db::file_scenarios::create;

        // Force-insert a forged binding: image/* → code_exec, is_default=true.
        // This bypasses the HTTP validation layer (which would normally reject it).
        // We use raw SQL to skip the DB CHECK on executor (which only allows tool|skill,
        // but code_exec with executor=tool is the threat model: a constant member that
        // is NOT in FSE_DEFAULT_ALLOWLIST).
        //
        // Since `code_exec` is executor=tool and NOT in FSE_DEFAULT_ALLOWLIST,
        // is_allowed_for_autorun("code_exec", &enabled) returns false regardless of toggle.
        //
        // We can use `create()` directly since executor=tool is valid for the DB CHECK;
        // the protection must come from the allowlist re-check at dispatch time.
        create(
            &pool,
            "image/*",
            "tool",
            "code_exec",      // NOT in FSE_DEFAULT_ALLOWLIST — forged action
            "Forged action",
            true,             // is_default = true (the threat: auto-runs 0-click)
            50,
            true,             // enabled in bindings table
            "attacker",
        )
        .await
        .expect("raw insert of forged binding must succeed at DB level");

        let server = MockServer::start().await;
        let jpeg_bytes: Vec<u8> = b"\xFF\xD8\xFFfakejpeg".to_vec();
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg_bytes))
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("0.0.0.0:{port}");
        let upload_url = format!(
            "{}/api/uploads/10000000-0000-0000-0000-000000000003?sig=x&exp=1",
            server.uri()
        );
        let image_att = MediaAttachment {
            url: upload_url.clone(),
            media_type: MediaType::Image,
            file_name: Some("attack.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            file_size: None,
        };

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, _pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            &server.uri(),
            "en",
            &pool,
            &mut enriched,
            &[image_att],
        )
        .await;

        assert_eq!(outcomes.len(), 1);
        // Must NOT be Unsupported (which would indicate dispatch reached code_exec
        // and resolve() caught it); must be Ok from `save` — the allowlist re-check
        // fires BEFORE dispatch_action is called.
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "forged code_exec default must be blocked at allowlist re-check → save fallback (Ok); \
             got: {:?} / {:?}",
            outcomes[0].status,
            outcomes[0].reason
        );
        assert!(
            outcomes[0].summary_text.contains("saved"),
            "forged default must resolve to save, not code_exec: {:?}",
            outcomes[0].summary_text
        );
    }

    // ── download failure → Failed outcome ─────────────────────────────────────

    #[sqlx::test(migrations = "../../migrations")]
    async fn download_failure_yields_failed_outcome(pool: sqlx::PgPool) {
        // Mock returns 500 → download_full returns None → Failed outcome.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(ResponseTemplate::new(500).set_body_bytes(b"err".to_vec()))
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");
        let upload_url = format!(
            "{}/api/uploads/00000000-0000-0000-0000-000000000005?sig=x&exp=1",
            server.uri()
        );

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, _pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            "http://localhost:9011",
            "ru",
            &pool,
            &mut enriched,
            &[att(&upload_url, MediaType::Document)],
        )
        .await;

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Failed
        );
    }

    // ── I-3: FSE_AUTO_RUN audit emit tests ────────────────────────────────────

    /// (a) When a default binding auto-runs (allowed by allowlist), an `fse_auto_run`
    /// row must appear in `audit_events`. The save-fallback path must NOT emit one.
    #[sqlx::test(migrations = "../../migrations")]
    async fn auto_run_default_emits_fse_auto_run_audit(pool: sqlx::PgPool) {
        use crate::db::file_scenarios::create;

        // Insert a default `save` binding for `application/octet-stream`.
        // `save` is in FSE_DEFAULT_ALLOWLIST → allowlist re-check passes → audit fires.
        create(
            &pool,
            "application/octet-stream",
            "tool",
            "save",
            "Save file",
            true, // is_default
            50,
            true, // enabled
            "test",
        )
        .await
        .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/uploads/.*"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"\x00\x01\x02\x03".to_vec()),
            )
            .mount(&server)
            .await;

        let port = server.address().port();
        let gateway_listen = format!("127.0.0.1:{port}");
        let upload_uuid = Uuid::new_v4();
        let upload_url = format!("{}/api/uploads/{upload_uuid}?sig=x&exp=1", server.uri());

        let client = reqwest::Client::new();
        let mut enriched = String::new();
        let (outcomes, _pending) = dispatch_attachments(
            &client,
            &gateway_listen,
            "http://localhost:9011",
            "en",
            &pool,
            &mut enriched,
            &[att(&upload_url, hydeclaw_types::MediaType::Document)],
        )
        .await;

        assert_eq!(outcomes.len(), 1, "one outcome");
        assert_eq!(
            outcomes[0].status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Ok,
            "save returns Ok"
        );

        // Give the spawned audit task a moment to complete.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Verify an fse_auto_run row was written to audit_events.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events WHERE event_type = 'fse_auto_run'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit_events query must succeed");

        assert_eq!(count, 1, "exactly one fse_auto_run audit row must be emitted");
    }

    /// (b) The save-fallback branch (0 bindings) and the allowlist-blocked branch
    /// must NOT emit an `fse_auto_run` audit row.
    #[sqlx::test(migrations = "../../migrations")]
    async fn save_fallback_and_blocked_do_not_emit_fse_auto_run_audit(pool: sqlx::PgPool) {
        use crate::agent::fse::set_enabled_allowlist;
        use crate::db::file_scenarios::create;

        // Case 1: 0-binding fallback to save (no default, no bindings at all).
        // No bindings → 0-binding branch → run_builtin("save") → no audit.
        {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path_regex(r"^/api/uploads/.*"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_bytes(b"\x00\x01\x02\x03".to_vec()),
                )
                .mount(&server)
                .await;

            let port = server.address().port();
            let gateway_listen = format!("127.0.0.1:{port}");
            let upload_url = format!(
                "{}/api/uploads/20000000-0000-0000-0000-000000000001?sig=x&exp=1",
                server.uri()
            );

            let client = reqwest::Client::new();
            let mut enriched = String::new();
            let (outcomes, _) = dispatch_attachments(
                &client,
                &gateway_listen,
                "http://localhost:9011",
                "en",
                &pool,
                &mut enriched,
                &[att(&upload_url, hydeclaw_types::MediaType::Document)],
            )
            .await;
            assert_eq!(outcomes.len(), 1, "one outcome for 0-binding case");
        }

        // Case 2: allowlist-blocked default → falls back to save → no audit.
        // Insert `describe` as is_default for image/*, then disable it so allowlist
        // re-check blocks it → action_to_run = "save" → no FSE_AUTO_RUN.
        {
            // Seed the `describe` default for image/*.
            create(
                &pool,
                "image/*",
                "tool",
                "describe",
                "Describe",
                true, // is_default
                50,
                true, // enabled in bindings
                "test",
            )
            .await
            .unwrap();

            // Disable `describe` in the allowlist so the re-check blocks it.
            set_enabled_allowlist(
                &pool,
                &["transcribe".to_string(), "extract_document".to_string(), "save".to_string()],
            )
            .await
            .expect("set_enabled_allowlist must not fail");

            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path_regex(r"^/api/uploads/.*"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(b"\xFF\xD8\xFFfakejpeg".to_vec()),
                )
                .mount(&server)
                .await;

            let port = server.address().port();
            let gateway_listen = format!("0.0.0.0:{port}");
            let upload_url = format!(
                "{}/api/uploads/20000000-0000-0000-0000-000000000002?sig=x&exp=1",
                server.uri()
            );
            let image_att = MediaAttachment {
                url: upload_url,
                media_type: hydeclaw_types::MediaType::Image,
                file_name: Some("blocked.jpg".into()),
                mime_type: Some("image/jpeg".into()),
                file_size: None,
            };

            let client = reqwest::Client::new();
            let mut enriched = String::new();
            let (outcomes, _) = dispatch_attachments(
                &client,
                &gateway_listen,
                "http://localhost:9011",
                "en",
                &pool,
                &mut enriched,
                &[image_att],
            )
            .await;
            assert_eq!(outcomes.len(), 1, "one outcome for blocked case");
        }

        // Give any potential spawned tasks a moment.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Neither the 0-binding fallback nor the blocked case should have emitted audit.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events WHERE event_type = 'fse_auto_run'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit_events query must succeed");

        assert_eq!(
            count,
            0,
            "save-fallback and allowlist-blocked must NOT emit fse_auto_run; got {count}"
        );
    }
}
