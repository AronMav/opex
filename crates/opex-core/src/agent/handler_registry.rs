//! Core-side discovery cache + matcher for toolgate-hosted file handlers.
//! `HandlerManifest` mirrors the toolgate `GET /handlers` item wire shape;
//! `match_buttons` is the pure tiered trust gate (builtin∩allowlist,
//! workspace default-on) that turns a mime+size into composer buttons.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use url::Url;

use crate::agent::fse::allowlist::FSE_DEFAULT_ALLOWLIST;

/// Inner `"match"` object of a manifest: mime globs + an optional size cap
/// + domain patterns for URL-based handler matching.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HandlerMatch {
    #[serde(default)]
    pub mime: Vec<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub max_size_mb: Option<u64>,
}

/// One handler manifest as served by toolgate `GET /handlers`. Serde field
/// names match the toolgate JSON; the nested object is read via
/// `#[serde(rename = "match")]` (Rust keyword → `match_`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandlerManifest {
    pub id: String,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub descriptions: HashMap<String, String>,
    #[serde(default)]
    pub icon: String,
    #[serde(rename = "match", default)]
    pub match_: HandlerMatch,
    #[serde(default)]
    pub capability: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub execution: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub params: serde_json::Value,
    /// Operator-configurable settings ("valves") declared in the handler's
    /// `<config>` descriptor block. Array of `{name,type,default,label,description}`.
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub source: String,
    /// Optional `<command name="..." aliases="a,b"/>` override from the
    /// handler's descriptor. Applied by `derive_handler_commands` to name the
    /// derived chat command — the handler id (enqueue target) is unaffected.
    #[serde(default)]
    pub command: Option<CommandOverride>,
}

/// Custom command name/aliases declared by a handler descriptor's
/// `<command>` tag, surfaced in the toolgate `/handlers` JSON as
/// `"command": {"name": "...", "aliases": [...]}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandOverride {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// A composer button derived from a manifest for a concrete file.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HandlerButton {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub params: serde_json::Value,
}

/// True if `mime` matches a glob like `audio/*`, the universal `*` / `*/*`, or
/// an exact `application/pdf`.
///
/// The universal wildcard is checked FIRST: `"*/*".strip_suffix("/*")` yields
/// `Some("*")`, so a naive suffix-first order would (wrongly) require the mime's
/// type segment to equal the literal `"*"` and never match a real file — which
/// silently disabled the `save` handler (mime `*/*`) for every upload.
fn mime_glob_matches(pattern: &str, mime: &str) -> bool {
    if pattern == "*" || pattern == "*/*" {
        true
    } else if let Some(prefix) = pattern.strip_suffix("/*") {
        mime.split('/').next() == Some(prefix)
    } else {
        pattern.eq_ignore_ascii_case(mime)
    }
}

/// Localize a manifest label: requested `lang`, else `en`, else the id.
fn localize(m: &HandlerManifest, lang: &str) -> String {
    m.labels
        .get(lang)
        .or_else(|| m.labels.get("en"))
        .cloned()
        .unwrap_or_else(|| m.id.clone())
}

/// Pure tiered match: filter manifests by mime-glob + `max_size_mb`, apply the
/// trust gate by tier (builtin → must be one of the 5 const ids AND an enabled
/// member of the allowlist; workspace → allowed by default), then localize +
/// sort by `order` then id.
pub fn match_buttons(
    manifests: &[HandlerManifest],
    mime: &str,
    size: u64,
    enabled_allowlist: &[String],
    lang: &str,
) -> Vec<HandlerButton> {
    let mut matched: Vec<&HandlerManifest> = manifests
        .iter()
        .filter(|m| m.match_.mime.iter().any(|p| mime_glob_matches(p, mime)))
        .filter(|m| match m.match_.max_size_mb {
            Some(cap) => size <= cap.saturating_mul(1024 * 1024),
            None => true,
        })
        .filter(|m| match m.tier.as_str() {
            "builtin" => {
                // builtin ids hard-anchored to the const; allowlist toggle gates which are on.
                FSE_DEFAULT_ALLOWLIST.contains(&m.id.as_str())
                    && enabled_allowlist.iter().any(|x| x == &m.id)
            }
            // workspace (and any future tier) → default-on for v1 trusted authors
            _ => true,
        })
        .collect();

    matched.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    matched
        .into_iter()
        .map(|m| HandlerButton {
            id: m.id.clone(),
            label: localize(m, lang),
            icon: m.icon.clone(),
            params: m.params.clone(),
        })
        .collect()
}

/// Retain only async-execution handlers in a button list (F070).
///
/// The model-driven menu (`file_handler` tool) and the menu-click endpoints
/// (`/api/files/run`, `/api/files/menu-run`) enqueue the chosen handler onto the
/// async-only `handler_jobs` queue. Sync handlers (describe / extract_document /
/// save) run INLINE via the composer's `/api/files/run` path instead;
/// offering or enqueuing one through the async menu path strands the job (no
/// `/complete` callback is ever posted). `match_url_handlers` already filters
/// this way — this applies the same guard to the upload path, which shares the
/// unfiltered `match_buttons` with the inline sync path and so cannot filter at
/// the source.
pub fn retain_async_handlers(buttons: &mut Vec<HandlerButton>, manifests: &[HandlerManifest]) {
    buttons.retain(|b| {
        manifests
            .iter()
            .any(|m| m.id == b.id && m.execution == "async")
    });
}

/// True if `url`'s host matches a domain pattern from a handler manifest.
///
/// Pattern matching rules:
/// - `"*"` matches any host (universal wildcard)
/// - `"youtube.com"` matches `youtube.com` and any subdomain (`www.youtube.com`)
/// - `"youtu.be"` matches `youtu.be` exactly (no subdomain matching for short domains)
/// - Case-insensitive
fn domain_matches(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let p = pattern.to_lowercase();
    let h = host.to_lowercase();
    if p == h {
        return true;
    }
    // Pattern without leading dot → match subdomains too
    if h.ends_with(&format!(".{p}")) {
        return true;
    }
    false
}

/// Match handlers that declare domains matching the given URL's host.
/// Same trust gate as `match_buttons`: builtin must be in allowlist,
/// workspace is default-on. Returns matching buttons sorted by order, id.
pub fn match_url_handlers(
    manifests: &[HandlerManifest],
    url: &str,
    enabled_allowlist: &[String],
    lang: &str,
) -> Vec<HandlerButton> {
    let parsed = match Url::parse(url) {
        Ok(u) => u,
        Err(_) => return vec![],
    };
    let host = match parsed.host_str() {
        Some(h) => h,
        None => return vec![],
    };

    let mut matched: Vec<&HandlerManifest> = manifests
        .iter()
        .filter(|m| !m.match_.domains.is_empty())
        // URL actions are dispatched via /api/handlers/enqueue, which is
        // async-only. Excluding sync handlers here stops a sync handler (e.g.
        // `transcribe`) from being offered as a URL button that 400s on click.
        .filter(|m| m.execution == "async")
        .filter(|m| m.match_.domains.iter().any(|d| domain_matches(d, host)))
        .filter(|m| match m.tier.as_str() {
            "builtin" => {
                FSE_DEFAULT_ALLOWLIST.contains(&m.id.as_str())
                    && enabled_allowlist.iter().any(|x| x == &m.id)
            }
            _ => true,
        })
        .collect();

    matched.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    matched
        .into_iter()
        .map(|m| HandlerButton {
            id: m.id.clone(),
            label: localize(m, lang),
            icon: m.icon.clone(),
            params: m.params.clone(),
        })
        .collect()
}

// ── HandlerRegistry ─────────────────────────────────────────────────────────

/// Cached state behind the registry lock.
#[derive(Default)]
pub struct HandlerCache {
    manifests: Vec<HandlerManifest>,
    etag: Option<String>,
}

/// Discovery cache of toolgate handler manifests. Refresh via conditional GET
/// (`If-None-Match` ETag); a 304 or a transport error keeps the prior cache
/// (fail-soft, so composer buttons still render when toolgate is briefly down).
#[derive(Clone)]
pub struct HandlerRegistry {
    inner: Arc<RwLock<HandlerCache>>,
    toolgate_url: String,
    http: reqwest::Client,
}

/// Top-level shape of the toolgate `GET /handlers` response.
#[derive(Deserialize)]
struct HandlersResponse {
    handlers: Vec<HandlerManifest>,
    #[serde(default)]
    etag: Option<String>,
}

impl HandlerRegistry {
    pub fn new(toolgate_url: String, http: reqwest::Client) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HandlerCache::default())),
            toolgate_url,
            http,
        }
    }

    /// Conditional GET of `{toolgate_url}/handlers`. 200 replaces the cache;
    /// 304 / any non-2xx / transport error / bad JSON leaves it untouched.
    pub async fn refresh(&self) {
        let url = format!("{}/handlers", self.toolgate_url.trim_end_matches('/'));
        let prior_etag = self.inner.read().await.etag.clone();
        let mut req = self.http.get(&url);
        if let Some(tag) = &prior_etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, tag.clone());
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "handler registry refresh failed; keeping cache");
                return;
            }
        };
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return;
        }
        if !resp.status().is_success() {
            tracing::warn!(status = %resp.status(), "handler registry refresh non-2xx; keeping cache");
            return;
        }
        let header_etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        match resp.json::<HandlersResponse>().await {
            Ok(parsed) => {
                let mut guard = self.inner.write().await;
                guard.manifests = parsed.handlers;
                guard.etag = header_etag.or(parsed.etag);
            }
            Err(e) => {
                tracing::warn!(error = %e, "handler registry bad JSON; keeping cache");
            }
        }
    }

    /// Snapshot of the cached manifests (clones the Vec — small, ≤ ~20 items).
    pub async fn manifests(&self) -> Vec<HandlerManifest> {
        self.inner.read().await.manifests.clone()
    }

    /// Current ETag from the last successful `refresh()` (F8 versioning).
    /// `None` before the first successful refresh or if toolgate never sent one.
    pub async fn etag(&self) -> Option<String> {
        self.inner.read().await.etag.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mf(id: &str, tier: &str, mimes: &[&str], max_mb: Option<u64>, order: i32) -> HandlerManifest {
        let mut labels = HashMap::new();
        labels.insert("ru".to_string(), format!("{id}-ru"));
        labels.insert("en".to_string(), format!("{id}-en"));
        HandlerManifest {
            id: id.to_string(),
            labels,
            descriptions: HashMap::new(),
            icon: "mic".to_string(),
            match_: HandlerMatch {
                mime: mimes.iter().map(|s| s.to_string()).collect(),
                domains: vec![],
                max_size_mb: max_mb,
            },
            capability: None,
            provider: None,
            execution: "sync".to_string(),
            output: "text".to_string(),
            params: serde_json::json!([]),
            config: serde_json::json!([]),
            order,
            tier: tier.to_string(),
            source: String::new(),
            command: None,
        }
    }

    fn full() -> Vec<String> {
        FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manifest_deserializes_from_toolgate_json() {
        let raw = serde_json::json!({
            "id": "transcribe",
            "labels": {"ru": "Транскрибировать", "en": "Transcribe"},
            "descriptions": {"ru": "речь в текст"},
            "icon": "mic",
            "match": {"mime": ["audio/*", "video/*"], "max_size_mb": 200},
            "capability": "stt",
            "provider": "speaches-local",
            "execution": "sync",
            "output": "text",
            "params": [{"name": "language", "type": "string", "default": "ru", "required": false}],
            "order": 10,
            "tier": "builtin"
        });
        let m: HandlerManifest = serde_json::from_value(raw).unwrap();
        assert_eq!(m.id, "transcribe");
        assert_eq!(m.match_.mime, vec!["audio/*".to_string(), "video/*".to_string()]);
        assert_eq!(m.match_.max_size_mb, Some(200));
        assert_eq!(m.tier, "builtin");
        assert_eq!(m.provider.as_deref(), Some("speaches-local"));
        assert_eq!(m.labels.get("ru").map(|s| s.as_str()), Some("Транскрибировать"));
    }

    #[test]
    fn manifest_defaults_missing_optional_fields() {
        // A minimal manifest (only id + execution) must deserialize with defaults.
        let raw = serde_json::json!({"id": "save", "execution": "sync"});
        let m: HandlerManifest = serde_json::from_value(raw).unwrap();
        assert_eq!(m.id, "save");
        assert!(m.match_.mime.is_empty());
        assert!(m.match_.max_size_mb.is_none());
        assert!(m.labels.is_empty());
        assert_eq!(m.order, 0);
    }

    #[test]
    fn retain_async_handlers_drops_sync_keeps_async() {
        // F070: the menu/enqueue path must offer async handlers only.
        let sync_m = mf("save", "builtin", &["*/*"], None, 10); // mf defaults to sync
        let mut async_m = mf("summarize_video", "workspace", &["video/*"], None, 20);
        async_m.execution = "async".to_string();
        let manifests = vec![sync_m, async_m];
        let btn = |id: &str| HandlerButton {
            id: id.to_string(),
            label: id.to_string(),
            icon: String::new(),
            params: serde_json::json!([]),
        };
        let mut buttons = vec![btn("save"), btn("summarize_video")];
        retain_async_handlers(&mut buttons, &manifests);
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].id, "summarize_video");
    }

    #[test]
    fn builtin_button_requires_allowlist_membership() {
        // builtin "transcribe" matches audio/* and is in the full allowlist → button
        let ms = vec![mf("transcribe", "builtin", &["audio/*"], Some(200), 10)];
        let out = match_buttons(&ms, "audio/ogg", 1_000, &full(), "ru");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "transcribe");
        assert_eq!(out[0].label, "transcribe-ru");

        // same builtin id, but operator disabled it in the toggle → hidden
        let enabled = vec!["describe".to_string()];
        let out2 = match_buttons(&ms, "audio/ogg", 1_000, &enabled, "ru");
        assert!(out2.is_empty(), "disabled builtin must not produce a button");
    }

    #[test]
    fn unknown_builtin_id_is_never_offered() {
        // tier=builtin but id not in the const FSE_DEFAULT_ALLOWLIST → never a button,
        // even if a hand-edited allowlist row somehow lists it.
        let ms = vec![mf("rm_rf", "builtin", &["audio/*"], None, 1)];
        let bogus_enabled = vec!["rm_rf".to_string()];
        assert!(match_buttons(&ms, "audio/ogg", 1, &bogus_enabled, "ru").is_empty());
    }

    #[test]
    fn workspace_button_is_default_on_ignoring_allowlist() {
        // a workspace-tier handler not in the allowlist still produces a button
        let ms = vec![mf("my_ocr", "workspace", &["image/*"], None, 5)];
        let out = match_buttons(&ms, "image/png", 1_000, &full(), "ru");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "my_ocr");
    }

    #[test]
    fn universal_glob_matches_any_mime() {
        // Regression: the `save` builtin declares mime `*/*` and must produce a
        // button for ANY file. A suffix-first matcher broke this (see
        // mime_glob_matches doc-comment) — 403 on run + no composer button.
        assert!(mime_glob_matches("*/*", "text/plain"));
        assert!(mime_glob_matches("*/*", "image/png"));
        assert!(mime_glob_matches("*/*", "application/pdf"));
        assert!(mime_glob_matches("*", "audio/ogg"));

        let ms = vec![mf("save", "builtin", &["*/*"], None, 1)];
        for mime in ["text/plain", "image/png", "application/pdf", "video/mp4"] {
            let out = match_buttons(&ms, mime, 100, &full(), "ru");
            assert_eq!(out.len(), 1, "save must match {mime}");
            assert_eq!(out[0].id, "save");
        }
    }

    #[test]
    fn non_matching_mime_and_oversize_are_excluded() {
        let ms = vec![mf("transcribe", "builtin", &["audio/*"], Some(1), 10)];
        // wrong mime
        assert!(match_buttons(&ms, "image/png", 1_000, &full(), "ru").is_empty());
        // 2 MB > 1 MB cap
        assert!(match_buttons(&ms, "audio/ogg", 2 * 1024 * 1024, &full(), "ru").is_empty());
        // within cap
        assert_eq!(match_buttons(&ms, "audio/ogg", 100, &full(), "ru").len(), 1);
    }

    #[test]
    fn buttons_sorted_by_order_and_label_falls_back_to_en() {
        let ms = vec![
            mf("describe", "workspace", &["image/*"], None, 20),
            mf("save", "workspace", &["image/*"], None, 10),
        ];
        // "fr" missing → falls back to "en"
        let out = match_buttons(&ms, "image/png", 1, &full(), "fr");
        assert_eq!(out.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), vec!["save", "describe"]);
        assert_eq!(out[0].label, "save-en");
    }

    #[tokio::test]
    async fn refresh_loads_then_keeps_cache_on_304() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "handlers": [{
                "id": "transcribe",
                "labels": {"ru": "Транскрибировать"},
                "icon": "mic",
                "match": {"mime": ["audio/*"], "max_size_mb": 200},
                "execution": "sync",
                "output": "text",
                "params": [],
                "order": 10,
                "tier": "builtin"
            }],
            "etag": "abc123"
        });
        // First GET → 200 with ETag.
        Mock::given(method("GET"))
            .and(path("/handlers"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"abc123\"")
                    .set_body_json(&body),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Subsequent GETs (conditional) → 304.
        Mock::given(method("GET"))
            .and(path("/handlers"))
            .respond_with(ResponseTemplate::new(304))
            .mount(&server)
            .await;

        let reg = HandlerRegistry::new(server.uri(), reqwest::Client::new());
        reg.refresh().await;
        let ms = reg.manifests().await;
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].id, "transcribe");

        // second refresh: server returns 304 → cache kept, manifests unchanged
        reg.refresh().await;
        assert_eq!(reg.manifests().await.len(), 1, "304 must keep prior cache");
    }

    #[tokio::test]
    async fn refresh_failsoft_keeps_cache_when_toolgate_down() {
        let reg = HandlerRegistry::new("http://127.0.0.1:1".to_string(), reqwest::Client::new());
        // never loaded; a failing refresh must not panic and leaves empty cache
        reg.refresh().await;
        assert!(reg.manifests().await.is_empty());
    }

    // ── domain_matches tests ────────────────────────────────────────────────

    #[test]
    fn domain_matches_exact_host() {
        assert!(domain_matches("youtube.com", "youtube.com"));
        assert!(domain_matches("youtu.be", "youtu.be"));
    }

    #[test]
    fn domain_matches_subdomain() {
        assert!(domain_matches("youtube.com", "www.youtube.com"));
        assert!(domain_matches("youtube.com", "m.youtube.com"));
        assert!(domain_matches("youtube.com", "sub.domain.youtube.com"));
    }

    #[test]
    fn domain_matches_case_insensitive() {
        assert!(domain_matches("youtube.com", "YouTube.com"));
        assert!(domain_matches("YouTube.com", "youtube.com"));
    }

    #[test]
    fn domain_rejects_non_subdomain() {
        assert!(!domain_matches("youtube.com", "notyoutube.com"));
        assert!(!domain_matches("youtube.com", "youtube.com.evil.com"));
        assert!(!domain_matches("youtu.be", "youtu.bad"));
    }

    #[test]
    fn domain_rejects_empty() {
        assert!(!domain_matches("youtube.com", ""));
        assert!(!domain_matches("", "youtube.com"));
    }

    #[test]
    fn domain_wildcard_matches_anything() {
        assert!(domain_matches("*", "youtube.com"));
        assert!(domain_matches("*", "www.youtube.com"));
        assert!(domain_matches("*", "example.org"));
    }

    // ── match_url_handlers tests ────────────────────────────────────────────

    fn full_allowlist() -> Vec<String> {
        FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    }

    fn manifest_with_domains(id: &str, domains: &[&str], tier: &str) -> HandlerManifest {
        HandlerManifest {
            id: id.to_string(),
            labels: [("en".to_string(), id.to_string())].into_iter().collect(),
            descriptions: HashMap::new(),
            icon: "video".to_string(),
            match_: HandlerMatch {
                mime: vec!["video/*".into()],
                domains: domains.iter().map(|s| s.to_string()).collect(),
                max_size_mb: Some(2000),
            },
            capability: None,
            provider: None,
            execution: "async".into(),
            output: "text".into(),
            params: serde_json::json!([]),
            config: serde_json::json!([]),
            order: 10,
            tier: tier.into(),
            source: String::new(),
            command: None,
        }
    }

    #[test]
    fn match_url_handlers_finds_youtube_handler() {
        let manifests = vec![
            manifest_with_domains("summarize_video", &["youtube.com", "youtu.be"], "builtin"),
            manifest_with_domains("transcribe", &["*"], "builtin"),
        ];
        let allowlist = full_allowlist();
        let buttons = match_url_handlers(&manifests, "https://www.youtube.com/watch?v=abc", &allowlist, "en");
        assert_eq!(buttons.len(), 2, "both handlers should match youtube URL");
        assert_eq!(buttons[0].id, "summarize_video");
    }

    #[test]
    fn match_url_handlers_no_match_for_random_url() {
        let manifests = vec![
            manifest_with_domains("summarize_video", &["youtube.com"], "builtin"),
        ];
        let allowlist = full_allowlist();
        let buttons = match_url_handlers(&manifests, "https://example.com/page", &allowlist, "en");
        assert!(buttons.is_empty());
    }

    #[test]
    fn match_url_handlers_respects_trust_gate() {
        let manifests = vec![
            manifest_with_domains("summarize_video", &["youtube.com"], "builtin"),
        ];
        // Empty allowlist → builtin handler filtered out
        let buttons = match_url_handlers(&manifests, "https://youtube.com/watch?v=abc", &[], "en");
        assert!(buttons.is_empty(), "empty allowlist should reject builtin");
    }

    #[test]
    fn match_url_handlers_invalid_url_returns_empty() {
        let manifests = vec![];
        let buttons = match_url_handlers(&manifests, "not-a-url", &[], "en");
        assert!(buttons.is_empty());
    }
}
