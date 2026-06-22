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
use crate::agent::url_tools::uploads_local_url;
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
                // ≥1 bindings and a default exists → run the highest-priority default.
                let outcome = run_builtin(
                    &b.action_ref,
                    http_client,
                    gateway_listen,
                    toolgate_url,
                    agent_language,
                    att,
                )
                .await;
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
}
