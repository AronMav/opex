//! Sync execution path for toolgate file handlers (`describe`, `extract_document`,
//! `save`).
//!
//! Used by two callers, both of which previously stranded sync handlers because
//! the async-only `handler_jobs` queue rejected them (F070):
//!   1. the model-driven `file_handler` system tool (`agent/tool_handlers/file_handler.rs`)
//!      — list now offers sync handlers, run dispatches them through this helper;
//!   2. the menu-click endpoint `POST /api/files/run` family
//!      (`gateway/handlers/files.rs::menu_run_core`) — same split.
//!
//! Async handlers (`transcribe`, `summarize_video`) bypass this and stay on the
//! `handler_jobs` queue + `/complete` callback path — they cannot return within
//! an HTTP request/tool-call window.
//!
//! The contract mirrors what the removed `POST /api/files/{upload_id}/run` did
//! (commit 3fb895b4 accidentally dropped it as "dead code" together with the
//! `/actions` sibling; this module revives only the sync execution body,
//! factored out so both the gateway handler and the LLM tool can share it).
//!
//! R12 (loopback×SSRF): toolgate never receives a loopback URL — Core downloads
//! the upload bytes here in Rust and POSTs them as multipart.

use crate::agent::file_scenario::outcome::ScenarioOutcome;

/// Process-wide pooled reqwest client for sync handler execution: a loopback
/// upload download plus a multipart POST to single-process toolgate. Built with
/// explicit timeouts (F028) so a slow/wedged handler can't pin a tokio task
/// plus the up-to-50MB buffered upload in memory — there is no global tower
/// TimeoutLayer on these internal calls.
///
/// `pool_max_idle_per_host(0)` disables keep-alive pooling — same rationale as
/// the embedding client (opex-embedding/src/client.rs): after a toolgate restart
/// the old connections in the pool are dead, and each retry would reuse the same
/// dead connection, waiting the full timeout. With pooling disabled, a dead
/// toolgate fails fast at `connect_timeout` (5s).
///
/// Mirrors `file_handler_worker`'s client but allows for the longer ceiling of
/// a sync PDF parse / vision description (300s vs 120s for the async dispatch).
pub(crate) fn http_client() -> &'static reqwest::Client {
    static SYNC_HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    SYNC_HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(300))
            .pool_max_idle_per_host(0)
            .tcp_keepalive(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build sync handler HTTP client: invalid timeout/pool configuration")
    })
}

/// Inputs for a sync handler run. The caller has already passed:
/// - owner-gate (`assert_upload_accessible`)
/// - tiered trust gate (`match_buttons` + chosen handler_id is in the matched set)
///
/// so this struct is just the execution context.
pub struct SyncRunRequest<'a> {
    pub upload_id: uuid::Uuid,
    pub handler_id: &'a str,
    pub agent: &'a str,
    pub mime: String,
    pub size: u64,
    pub language: &'a str,
    pub params: serde_json::Value,
}

/// Run a `execution == "sync"` toolgate handler inline and return its outcome.
///
/// Steps (mirrors the legacy `run_file_handler` body, factored out):
///   1. Mint a loopback signed URL; download bytes in Rust (R12 — toolgate never
///      sees this URL; its SSRF guard would reject it).
///   2. Resolve operator-set per-agent config ("valves") for `ctx.config`.
///   3. POST `multipart/form-data` to `{toolgate_url}/handlers/{id}/run` with the
///      `file` part + `mime` / `filename` / `size` / `params` / `config` /
///      `language` text fields.
///   4. Parse the JSON body as `ScenarioOutcome` and return it. Network / parse
///      errors become `ScenarioOutcome::failed(...)` so the caller can surface
///      them in the chat deterministically (same posture as the async path).
///
/// `signed_url_ttl_secs` is the TTL for the loopback signed URL (independent of
/// the per-job callback TTL — sync runs finish within the HTTP request, so the
/// operator-configured upload TTL is correct here).
#[allow(clippy::too_many_arguments)]
pub async fn run_sync_handler_inline(
    db: &sqlx::PgPool,
    http: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    signed_url_base: &str,
    key: &[u8; 32],
    signed_url_ttl_secs: u64,
    req: SyncRunRequest<'_>,
) -> ScenarioOutcome {
    // ── 1. Loopback download of the upload bytes ─────────────────────────────
    let web_url = crate::uploads::mint_uploads_url(
        signed_url_base,
        req.upload_id,
        key,
        signed_url_ttl_secs,
    );
    let loopback = crate::agent::url_tools::uploads_local_url(&web_url, gateway_listen);
    let bytes = match http.get(&loopback).send().await {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    error = %e, upload_id = %req.upload_id,
                    "sync_handler: failed to read upload bytes"
                );
                return ScenarioOutcome::failed(format!("upload read error: {e}"));
            }
        },
        Ok(r) => {
            tracing::warn!(
                status = %r.status(), upload_id = %req.upload_id,
                "sync_handler: loopback download non-2xx"
            );
            return ScenarioOutcome::failed(format!(
                "upload fetch failed: HTTP {}",
                r.status().as_u16()
            ));
        }
        Err(e) => {
            tracing::warn!(
                error = %e, upload_id = %req.upload_id,
                "sync_handler: loopback download failed"
            );
            return ScenarioOutcome::failed(format!("upload fetch error: {e}"));
        }
    };

    // ── 2. Per-agent config ("valves") → ctx.config ──────────────────────────
    let config_str = crate::db::handler_config::get_config(db, req.handler_id, req.agent)
        .await
        .ok()
        .and_then(|v| serde_json::to_string(&v).ok())
        .unwrap_or_else(|| "{}".to_string());
    let params_str = serde_json::to_string(&req.params).unwrap_or_else(|_| "{}".to_string());

    // ── 3. POST multipart/form-data to toolgate /handlers/{id}/run ───────────
    let url = format!(
        "{}/handlers/{}/run",
        toolgate_url.trim_end_matches('/'),
        req.handler_id
    );
    let file_part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(req.upload_id.to_string())
        .mime_str(&req.mime)
        .unwrap_or_else(|_| reqwest::multipart::Part::bytes(bytes.to_vec()));
    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("mime", req.mime.clone())
        .text("filename", req.upload_id.to_string())
        .text("size", req.size.to_string())
        .text("params", params_str)
        .text("config", config_str)
        .text("language", req.language.to_string());

    // ── 4. Parse the JSON body as ScenarioOutcome ────────────────────────────
    match http.post(&url).multipart(form).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<ScenarioOutcome>().await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(
                    error = %e, handler = %req.handler_id,
                    "sync_handler: toolgate returned bad JSON"
                );
                ScenarioOutcome::failed(format!("toolgate bad JSON: {e}"))
            }
        },
        Ok(resp) => {
            let code = resp.status().as_u16();
            tracing::warn!(
                status = code, handler = %req.handler_id,
                "sync_handler: toolgate non-2xx"
            );
            ScenarioOutcome::failed(format!("toolgate HTTP {code}"))
        }
        Err(e) => {
            tracing::warn!(
                error = %e, handler = %req.handler_id,
                "sync_handler: toolgate request failed"
            );
            ScenarioOutcome::failed(format!("toolgate request error: {e}"))
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A lazy pool that never actually connects — used in non-DB unit tests
    /// where the handler_config lookup falls back to "{}" on any DB error.
    fn lazy_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("connect_lazy must not fail — no actual connection is made here")
    }

    fn req<'a>(upload_id: uuid::Uuid, handler_id: &'a str) -> SyncRunRequest<'a> {
        SyncRunRequest {
            upload_id,
            handler_id,
            agent: "Atlas",
            mime: "application/json".to_string(),
            size: 42,
            language: "ru",
            params: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn sync_run_posts_multipart_and_parses_outcome() {
        // Upload server returns the bytes; core re-POSTs them to toolgate.
        let uploads = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"{\"json\":\"body\"}".to_vec()))
            .mount(&uploads)
            .await;

        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/extract_document/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "summary_text": "распознанный текст",
                "artifact_urls": [],
                "reason": null,
            })))
            .mount(&toolgate)
            .await;

        let listen = uploads.uri().trim_start_matches("http://").to_string();
        let http = reqwest::Client::new();
        let key = [9u8; 32];
        let outcome = run_sync_handler_inline(
            &lazy_pool(),
            &http,
            &toolgate.uri(),
            &listen,
            "", // root-relative signed_url_base
            &key,
            300,
            req(uuid::Uuid::new_v4(), "extract_document"),
        )
        .await;

        assert_eq!(outcome.status, crate::agent::file_scenario::outcome::ScenarioStatus::Ok);
        assert_eq!(outcome.summary_text, "распознанный текст");
    }

    #[tokio::test]
    async fn sync_run_returns_failed_on_toolgate_non_2xx() {
        let uploads = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"contents".to_vec()))
            .mount(&uploads)
            .await;

        let toolgate = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/handlers/save/run"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&toolgate)
            .await;

        let listen = uploads.uri().trim_start_matches("http://").to_string();
        let outcome = run_sync_handler_inline(
            &lazy_pool(),
            &reqwest::Client::new(),
            &toolgate.uri(),
            &listen,
            "",
            &[3u8; 32],
            300,
            req(uuid::Uuid::new_v4(), "save"),
        )
        .await;

        assert_eq!(
            outcome.status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Failed
        );
        assert!(outcome.reason.as_deref().unwrap_or("").contains("500"));
    }

    #[tokio::test]
    async fn sync_run_returns_failed_when_loopback_download_errors() {
        // No upload mock — any GET returns 404 → outcome.failed("upload fetch …").
        let uploads = MockServer::start().await;

        let toolgate = MockServer::start().await;
        // Toolgate should never be hit on the failure path.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status":"ok","summary_text":"unreached","artifact_urls":[],"reason":null
            })))
            .mount(&toolgate)
            .await;

        let listen = uploads.uri().trim_start_matches("http://").to_string();
        let outcome = run_sync_handler_inline(
            &lazy_pool(),
            &reqwest::Client::new(),
            &toolgate.uri(),
            &listen,
            "",
            &[1u8; 32],
            300,
            req(uuid::Uuid::new_v4(), "describe"),
        )
        .await;

        assert_eq!(
            outcome.status,
            crate::agent::file_scenario::outcome::ScenarioStatus::Failed
        );
        assert!(outcome.reason.as_deref().unwrap_or("").contains("upload fetch"));
    }
}
