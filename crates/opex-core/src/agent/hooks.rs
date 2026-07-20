//! Event hook system — synchronous only (no async DB/HTTP calls inside hooks).
//!
//! Hooks intercept engine events for policy enforcement, logging, and argument modification.
//! Use hooks for automated blocking; use the approval system for human-in-the-loop.

#[derive(Debug, Clone)]
#[allow(dead_code)] // AfterResponse and OnError are part of the hook-event API surface
                    // but nothing emits them today — kept for future extension.
pub enum HookEvent {
    BeforeMessage,
    AfterResponse,
    BeforeToolCall { agent: String, tool_name: String },
    AfterToolResult { agent: String, tool_name: String, duration_ms: u64 },
    OnError,
}

#[derive(Debug, Clone)]
pub enum HookAction {
    /// Continue normal execution.
    Continue,
    /// Block execution with reason.
    Block(String),
}

pub type HookHandler = Box<dyn Fn(&HookEvent) -> HookAction + Send + Sync>;

/// True when the cumulative decision-webhook chain has spent its budget.
/// A `budget_ms` of 0 disables the limit.
fn webhook_chain_exceeded(elapsed: std::time::Duration, budget_ms: u64) -> bool {
    budget_ms != 0 && (elapsed.as_millis() as u64) >= budget_ms
}

/// Compiled webhook entry: config + pre-compiled tool_matcher regex.
pub(crate) struct CompiledWebhook {
    pub cfg: crate::config::WebhookConfig,
    pub matcher: Option<regex::Regex>,
}

pub struct HookRegistry {
    handlers: Vec<(String, HookHandler)>,
    /// SSRF-safe client for async webhooks (set by set_webhooks).
    http_client: Option<reqwest::Client>,
    /// Plain client (no SSRF resolver) for allow_internal decision hooks.
    http_client_internal: Option<reqwest::Client>,
    webhooks: Vec<CompiledWebhook>,
    total_webhook_timeout_ms: u64,
    on_chain_timeout: crate::config::FailureMode,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            http_client: None,
            http_client_internal: None,
            webhooks: Vec::new(),
            total_webhook_timeout_ms: 10_000,
            on_chain_timeout: crate::config::FailureMode::Open,
        }
    }

    pub fn register(&mut self, name: String, handler: HookHandler) {
        tracing::info!(hook = %name, "hook registered");
        self.handlers.push((name, handler));
    }

    /// Fire event through all handlers. First non-Continue action wins.
    pub fn fire(&self, event: &HookEvent) -> HookAction {
        for (name, handler) in &self.handlers {
            match handler(event) {
                HookAction::Continue => continue,
                action => {
                    tracing::debug!(hook = %name, event = ?std::mem::discriminant(event), "hook intercepted");
                    return action;
                }
            }
        }
        HookAction::Continue
    }

    /// List registered hook names.
    #[allow(dead_code)] // sole caller was the removed GET /api/agents/{name}/hooks.
    pub fn names(&self) -> Vec<&str> {
        self.handlers.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Configure outbound webhook delivery. Pass an SSRF-safe reqwest::Client
    /// for async hooks; a plain client is built internally when any
    /// `allow_internal` + `Decision`-mode hook is present.
    pub fn set_webhooks(
        &mut self,
        client: reqwest::Client,
        webhooks: Vec<crate::config::WebhookConfig>,
    ) {
        if !webhooks.is_empty() {
            tracing::info!(count = webhooks.len(), "webhook hooks configured");
        }
        let needs_internal = webhooks.iter()
            .any(|w| w.allow_internal && matches!(w.mode, crate::config::WebhookMode::Decision));
        self.http_client = Some(client);
        if needs_internal {
            // Plain client (no SSRF resolver) for admin-opted-in internal hooks.
            self.http_client_internal = reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok();
        } else {
            // Hot-reload: if no internal hooks remain, reset the stale client.
            self.http_client_internal = None;
        }
        self.webhooks = webhooks.into_iter().map(|cfg| {
            let matcher = cfg.tool_matcher.as_ref().and_then(|p| {
                match regex::Regex::new(p) {
                    Ok(re) => Some(re),
                    Err(e) => {
                        tracing::warn!(pattern = %p, error = %e, "invalid hook tool_matcher; ignoring");
                        None
                    }
                }
            });
            CompiledWebhook { cfg, matcher }
        }).collect();
    }

    /// Set the cumulative decision-webhook chain budget. `total = None` → 10 s default;
    /// `Some(0)` → disabled.
    pub fn set_webhook_chain_budget(&mut self, total: Option<u64>, on_chain: crate::config::FailureMode) {
        self.total_webhook_timeout_ms = total.unwrap_or(10_000);
        self.on_chain_timeout = on_chain;
    }

    /// Run decision-webhooks sequentially for `event`. `extra` carries event-specific
    /// data: `{"tool_input": <args>}` (BeforeToolCall), `{"result": <str>}`
    /// (AfterToolResult), `{"message": <str>}` (BeforeMessage). Webhooks matching
    /// the event (and tool_matcher) run in order: first Block short-circuits;
    /// ModifyArgs / TransformResult / InjectContext chain across hooks.
    pub async fn fire_decision(&self, event: &HookEvent, extra: serde_json::Value) -> HookDecision {
        let chain_start = std::time::Instant::now();
        let ev_name = event_name(event);
        let tool = event_tool_name(event);

        let mut cur_extra = extra;
        let mut modified_args: Option<serde_json::Value> = None;
        let mut transformed: Option<String> = None;
        let mut injected: Vec<String> = Vec::new();

        for cw in self.webhooks.iter()
            .filter(|c| matches!(c.cfg.mode, crate::config::WebhookMode::Decision))
        {
            // filter by subscribed events
            if !cw.cfg.events.iter().any(|e| e == ev_name) { continue; }
            // filter by tool_matcher (only when matcher present AND tool name known)
            if let Some(re) = &cw.matcher {
                match tool {
                    Some(t) if re.is_match(t) => {} // passes
                    _ => continue,                   // no tool name or no match → skip
                }
            }

            let client = if cw.cfg.allow_internal {
                self.http_client_internal.as_ref()
            } else {
                self.http_client.as_ref()
            };
            let Some(client) = client else { continue; };

            if webhook_chain_exceeded(chain_start.elapsed(), self.total_webhook_timeout_ms) {
                tracing::warn!(budget_ms = self.total_webhook_timeout_ms, "decision webhook chain budget exceeded");
                match self.on_chain_timeout {
                    crate::config::FailureMode::Open => break, // fall through to the accumulation tail
                    crate::config::FailureMode::Closed => {
                        return HookDecision::Block("webhook chain budget exceeded".into());
                    }
                }
            }

            // Build request body: event fields + current extra.
            let agent_val = match event {
                HookEvent::BeforeToolCall { agent, .. }
                | HookEvent::AfterToolResult { agent, .. } => agent.clone(),
                _ => String::new(),
            };
            let mut req = serde_json::json!({ "event": ev_name, "agent": agent_val });
            if let Some(t) = tool { req["tool_name"] = serde_json::json!(t); }
            if let (Some(obj), Some(ex)) = (req.as_object_mut(), cur_extra.as_object()) {
                for (k, v) in ex { obj.insert(k.clone(), v.clone()); }
            }

            let host = reqwest::Url::parse(&cw.cfg.url).ok()
                .and_then(|u| u.host_str().map(|s| s.to_string()))
                .unwrap_or_default();

            // F043: wrap BOTH send() AND the body read in the timeout. The
            // allow_internal client has only a connect_timeout (no overall
            // .timeout()), and the previous timeout covered only send(), so a
            // decision webhook that returned headers promptly but stalled the
            // body hung r.text() indefinitely — freezing the tool decision and
            // the whole agent turn.
            let url = cw.cfg.url.clone();
            let fut = async {
                let r = client.post(&url).json(&req).send().await?;
                r.text().await
            };
            let resp = tokio::time::timeout(
                std::time::Duration::from_millis(cw.cfg.timeout_ms.min(30_000)),
                fut,
            ).await;

            let body = match resp {
                Ok(Ok(text)) => text,
                _ => {
                    tracing::warn!(url = %cw.cfg.url, "decision hook failed (timeout or transport error)");
                    match cw.cfg.on_failure {
                        crate::config::FailureMode::Open => continue,
                        crate::config::FailureMode::Closed => {
                            return HookDecision::Block("hook unavailable".into());
                        }
                    }
                }
            };

            match parse_decision(&body, event) {
                HookDecision::Block(r) => return HookDecision::Block(r),
                HookDecision::ModifyArgs(v) => {
                    cur_extra["tool_input"] = v.clone();
                    modified_args = Some(v);
                }
                HookDecision::TransformResult(s) => {
                    let tagged = hook_provenance(&host, &s);
                    cur_extra["result"] = serde_json::json!(tagged.clone());
                    transformed = Some(tagged);
                }
                HookDecision::InjectContext(s) => {
                    injected.push(hook_provenance(&host, &s));
                }
                HookDecision::Continue => {}
            }
        }

        if let Some(v) = modified_args { return HookDecision::ModifyArgs(v); }
        if let Some(s) = transformed { return HookDecision::TransformResult(s); }
        if !injected.is_empty() { return HookDecision::InjectContext(injected.join("\n")); }
        HookDecision::Continue
    }

    /// Fire matching async webhooks for `event`. Always returns immediately; the
    /// HTTP POST is dispatched on a detached `tokio::spawn` task with a
    /// 5-second timeout. Errors are logged at warn level and dropped — they
    /// NEVER alter HookAction (the existing `fire()` already returned for
    /// the synchronous policy decision).
    ///
    /// Only webhooks with `mode == Async` are dispatched here. Decision-mode
    /// webhooks are handled separately by `fire_decision`.
    pub fn fire_webhooks(&self, event: &HookEvent) {
        if self.webhooks.is_empty() { return; }
        let Some(client) = self.http_client.clone() else { return; };
        let ev_name = event_name(event);
        let timestamp = chrono::Utc::now().to_rfc3339();
        let (agent_field, tool_name_field, duration_ms_field) = match event {
            HookEvent::BeforeToolCall { agent, tool_name } =>
                (Some(agent.clone()), Some(tool_name.clone()), None),
            HookEvent::AfterToolResult { agent, tool_name, duration_ms } =>
                (Some(agent.clone()), Some(tool_name.clone()), Some(*duration_ms)),
            _ => (None, None, None),
        };
        for wh in self.webhooks.iter()
            .filter(|c| matches!(c.cfg.mode, crate::config::WebhookMode::Async))
            .map(|c| &c.cfg)
        {
            if !wh.events.iter().any(|e| e == ev_name) { continue; }
            let url = wh.url.clone();
            let client = client.clone();
            let body = serde_json::json!({
                "event": ev_name,
                "agent": agent_field,
                "tool_name": tool_name_field,
                "duration_ms": duration_ms_field,
                "timestamp": timestamp,
            });
            tokio::spawn(async move {
                let res = client
                    .post(&url)
                    .timeout(std::time::Duration::from_secs(5))
                    .json(&body)
                    .send()
                    .await;
                match res {
                    Ok(r) if r.status().is_success() => {
                        tracing::debug!(url = %url, status = %r.status(), "webhook delivered");
                    }
                    Ok(r) => {
                        tracing::warn!(url = %url, status = %r.status(), "webhook returned non-2xx");
                    }
                    Err(e) => {
                        tracing::warn!(url = %url, error = %e, "webhook delivery failed");
                    }
                }
            });
        }
    }
}

// ── Provenance sanitizer ─────────────────────────────────────────────────────

/// Prefix a webhook response body with a `[hook:{host}]` provenance marker
/// and neutralize any spoofed `[hook:` markers already present in the body
/// by inserting a zero-width space after `[hook`.
pub(crate) fn hook_provenance(host: &str, body: &str) -> String {
    let sanitized = body.replace("[hook:", "[hook\u{200b}:");
    format!("[hook:{host}] {sanitized}")
}

// ── #[cfg(test)] helpers on HookRegistry ─────────────────────────────────────

#[cfg(test)]
impl HookRegistry {
    /// Returns true if a plain (non-SSRF) internal client was built.
    pub(crate) fn has_internal_client(&self) -> bool {
        self.http_client_internal.is_some()
    }

    /// Returns true if the first compiled webhook has a matcher that matches `tool`.
    #[cfg(test)]
    pub(crate) fn first_matcher_matches(&self, tool: &str) -> bool {
        self.webhooks
            .first()
            .and_then(|c| c.matcher.as_ref())
            .map(|re| re.is_match(tool))
            .unwrap_or(false)
    }
}

// ── HookDecision + webhook-response parsing ─────────────────────────────────

/// Result of an async decision-webhook (richer than the sync HookAction).
#[derive(Debug, Clone)]
pub enum HookDecision {
    Continue,
    Block(String),
    ModifyArgs(serde_json::Value),
    InjectContext(String),
    TransformResult(String),
}

#[derive(serde::Deserialize, Default)]
struct WebhookResponse {
    decision: Option<String>,
    reason: Option<String>,
    inject_context: Option<String>,
    modified_args: Option<serde_json::Value>,
    transformed_result: Option<String>,
}

/// Return the tool name from events that carry one, or None.
pub(crate) fn event_tool_name(event: &HookEvent) -> Option<&str> {
    match event {
        HookEvent::BeforeToolCall { tool_name, .. }
        | HookEvent::AfterToolResult { tool_name, .. } => Some(tool_name),
        _ => None,
    }
}

/// Parse a webhook JSON body into a HookDecision. Lenient: invalid JSON or `{}`
/// → Continue (the caller applies on_failure for transport errors separately).
/// Precedence: explicit block > modified_args > transformed_result > inject_context > continue.
pub(crate) fn parse_decision(body: &str, event: &HookEvent) -> HookDecision {
    let r: WebhookResponse = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => return HookDecision::Continue,
    };
    if r.decision.as_deref() == Some("block") {
        return HookDecision::Block(r.reason.unwrap_or_else(|| "blocked by hook".into()));
    }
    if let Some(args) = r.modified_args
        && matches!(event, HookEvent::BeforeToolCall { .. }) && args.is_object()
    {
        return HookDecision::ModifyArgs(args);
    }
    if let Some(res) = r.transformed_result
        && matches!(event, HookEvent::AfterToolResult { .. })
    {
        return HookDecision::TransformResult(res);
    }
    if let Some(ctx) = r.inject_context
        && matches!(event, HookEvent::BeforeMessage)
    {
        return HookDecision::InjectContext(ctx);
    }
    HookDecision::Continue
}

// ── Map HookEvent variant to its canonical TOML name ────────────────────────

/// Map HookEvent variant to its canonical TOML name.
pub(crate) fn event_name(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::BeforeMessage => "BeforeMessage",
        HookEvent::AfterResponse => "AfterResponse",
        HookEvent::BeforeToolCall { .. } => "BeforeToolCall",
        HookEvent::AfterToolResult { .. } => "AfterToolResult",
        HookEvent::OnError => "OnError",
    }
}

/// Returns true if `event` matches the webhook's subscribed event list.
/// Used in tests; marked cfg(test) to silence dead_code when no callers exist in prod code.
#[cfg(test)]
pub(crate) fn webhook_matches(wh: &crate::config::WebhookConfig, event: &HookEvent) -> bool {
    let n = event_name(event);
    wh.events.iter().any(|e| e == n)
}

/// Built-in hook: log all tool calls via tracing.
pub fn logging_hook() -> HookHandler {
    Box::new(|event| {
        if let HookEvent::BeforeToolCall { agent, tool_name } = event {
            tracing::info!(agent = %agent, tool = %tool_name, "hook: tool call");
        }
        if let HookEvent::AfterToolResult { agent, tool_name, duration_ms } = event {
            tracing::info!(agent = %agent, tool = %tool_name, duration_ms, "hook: tool result");
        }
        HookAction::Continue
    })
}

/// Built-in hook: block specific tools by name.
pub fn block_tools_hook(blocked: Vec<String>) -> HookHandler {
    Box::new(move |event| {
        if let HookEvent::BeforeToolCall { tool_name, .. } = event
            && blocked.iter().any(|b| b == tool_name) {
                return HookAction::Block(format!("tool '{tool_name}' is blocked by policy"));
            }
        HookAction::Continue
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FailureMode, HooksConfig, WebhookConfig, WebhookMode};
    use std::time::Duration;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};

    // ── Decision-hook test helper ────────────────────────────────────────────

    fn decision_hook(url: String, matcher: Option<String>, on_failure: FailureMode) -> WebhookConfig {
        WebhookConfig {
            url,
            events: vec![
                "BeforeToolCall".into(),
                "AfterToolResult".into(),
                "BeforeMessage".into(),
            ],
            mode: WebhookMode::Decision,
            tool_matcher: matcher,
            on_failure,
            timeout_ms: 3000,
            allow_internal: true, // localhost WireMock → bypass SSRF
        }
    }

    // ── Test 1 — TOML parse ──────────────────────────────────────────────────

    #[test]
    fn toml_parse_webhooks() {
        let toml_str = r#"
log_all_tool_calls = false
block_tools = []

[[webhooks]]
url = "https://example.com/hook"
events = ["BeforeToolCall", "AfterToolResult"]
"#;
        let hc: HooksConfig = toml::from_str(toml_str).expect("should parse");
        assert_eq!(hc.webhooks.len(), 1);
        assert_eq!(hc.webhooks[0].url, "https://example.com/hook");
        assert_eq!(
            hc.webhooks[0].events,
            vec!["BeforeToolCall".to_string(), "AfterToolResult".to_string()]
        );
    }

    // ── Test 2 — Default empty webhooks ─────────────────────────────────────

    #[test]
    fn default_empty_webhooks() {
        assert!(HooksConfig::default().webhooks.is_empty());

        let toml_str = r#"
log_all_tool_calls = false
block_tools = []
"#;
        let hc: HooksConfig = toml::from_str(toml_str).expect("should parse without webhooks");
        assert!(hc.webhooks.is_empty());
    }

    // ── Test 3 — Event name match ────────────────────────────────────────────

    #[test]
    fn event_name_match() {
        let wh = WebhookConfig {
            url: "http://invalid.localhost.invalid:1/".into(),
            events: vec!["BeforeToolCall".into()],
            ..Default::default()
        };
        assert!(webhook_matches(
            &wh,
            &HookEvent::BeforeToolCall { agent: "a".into(), tool_name: "t".into() }
        ));
        assert!(!webhook_matches(&wh, &HookEvent::OnError));
        assert!(!webhook_matches(&wh, &HookEvent::BeforeMessage));
        assert!(!webhook_matches(&wh, &HookEvent::AfterResponse));
        assert!(!webhook_matches(
            &wh,
            &HookEvent::AfterToolResult { agent: "a".into(), tool_name: "t".into(), duration_ms: 1 }
        ));
    }

    #[test]
    fn event_name_mapping() {
        assert_eq!(event_name(&HookEvent::BeforeMessage), "BeforeMessage");
        assert_eq!(event_name(&HookEvent::AfterResponse), "AfterResponse");
        assert_eq!(
            event_name(&HookEvent::BeforeToolCall { agent: "a".into(), tool_name: "t".into() }),
            "BeforeToolCall"
        );
        assert_eq!(
            event_name(&HookEvent::AfterToolResult { agent: "a".into(), tool_name: "t".into(), duration_ms: 10 }),
            "AfterToolResult"
        );
        assert_eq!(event_name(&HookEvent::OnError), "OnError");
    }

    // ── Test 4 — fire_webhooks is fire-and-forget ────────────────────────────

    #[tokio::test]
    async fn fire_webhooks_is_fire_and_forget() {
        let mut registry = HookRegistry::new();
        registry.set_webhooks(
            reqwest::Client::builder().use_rustls_tls().build().unwrap(),
            vec![WebhookConfig {
                url: "http://127.0.0.1:1/never".into(),
                events: vec!["BeforeMessage".into()],
                ..Default::default()
            }],
        );
        // fire_webhooks must return before the 100ms timeout — it spawns and returns immediately
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            async { registry.fire_webhooks(&HookEvent::BeforeMessage) },
        )
        .await;
        assert!(result.is_ok(), "fire_webhooks must return before 100ms timeout");
    }

    // ── Test 5 — Empty webhooks no-op ────────────────────────────────────────

    #[tokio::test]
    async fn empty_webhooks_noop() {
        let registry = HookRegistry::new();
        // Must not panic, must not block
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            async { registry.fire_webhooks(&HookEvent::BeforeMessage) },
        )
        .await;
        assert!(result.is_ok(), "empty fire_webhooks must return immediately");
    }

    // ── Test 6 — parse_decision variants ────────────────────────────────────

    #[test]
    fn parse_decision_variants() {
        let btc = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "t".into() };
        let bm = HookEvent::BeforeMessage;
        let atr = HookEvent::AfterToolResult { agent: "A".into(), tool_name: "t".into(), duration_ms: 1 };

        // block
        assert!(matches!(
            parse_decision(r#"{"decision":"block","reason":"no"}"#, &btc),
            HookDecision::Block(r) if r == "no"));
        // empty → continue
        assert!(matches!(parse_decision("{}", &btc), HookDecision::Continue));
        // continue explicit
        assert!(matches!(parse_decision(r#"{"decision":"continue"}"#, &btc), HookDecision::Continue));
        // modified_args (BeforeToolCall)
        assert!(matches!(
            parse_decision(r#"{"modified_args":{"x":1}}"#, &btc),
            HookDecision::ModifyArgs(_)));
        // inject_context (BeforeMessage)
        assert!(matches!(
            parse_decision(r#"{"inject_context":"hi"}"#, &bm),
            HookDecision::InjectContext(s) if s == "hi"));
        // transformed_result (AfterToolResult)
        assert!(matches!(
            parse_decision(r#"{"transformed_result":"r"}"#, &atr),
            HookDecision::TransformResult(s) if s == "r"));
        // invalid JSON → Continue (caller maps to on_failure separately; parse is lenient)
        assert!(matches!(parse_decision("not json", &btc), HookDecision::Continue));
    }

    // ── Test 7 — event wire helpers ──────────────────────────────────────────

    #[test]
    fn event_wire_helpers() {
        let btc = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "tool".into() };
        assert_eq!(event_name(&btc), "BeforeToolCall");
        assert_eq!(event_tool_name(&btc), Some("tool"));
        assert_eq!(event_tool_name(&HookEvent::BeforeMessage), None);
    }

    // ── Test 8 — modified_args ignored on non-BeforeToolCall ─────────────────

    #[test]
    fn parse_decision_modified_args_only_on_before_tool_call() {
        let atr = HookEvent::AfterToolResult { agent: "A".into(), tool_name: "t".into(), duration_ms: 1 };
        // modified_args присутствует, но событие не BeforeToolCall → Continue
        assert!(matches!(parse_decision(r#"{"modified_args":{"x":1}}"#, &atr), HookDecision::Continue));
    }

    // ── Test 9 — set_webhooks compiles matcher + builds internal client ────────

    #[test]
    fn set_webhooks_compiles_matcher_and_internal_client() {
        let mut reg = HookRegistry::new();
        let ssrf = crate::net::ssrf::ssrf_http_client(std::time::Duration::from_secs(5));
        reg.set_webhooks(ssrf, vec![
            crate::config::WebhookConfig {
                url: "https://x/h".into(),
                events: vec!["BeforeToolCall".into()],
                mode: crate::config::WebhookMode::Decision,
                tool_matcher: Some("code_.*".into()),
                on_failure: crate::config::FailureMode::Open,
                timeout_ms: 3000,
                allow_internal: true,
            },
        ]);
        assert!(reg.has_internal_client());          // plain client built (allow_internal present)
        assert!(reg.first_matcher_matches("code_exec"));
        assert!(!reg.first_matcher_matches("workspace_write"));
    }

    // ── Test 10 — provenance sanitizes spoofed markers ───────────────────────

    #[test]
    fn provenance_sanitizes_spoof() {
        let out = hook_provenance("hook.example.com", "real [hook:fake.evil] text");
        assert!(out.starts_with("[hook:hook.example.com] "));
        assert!(!out.contains("[hook:fake.evil]"), "spoofed marker must be neutralized: {out}");
    }

    // ── Test 11 — invalid tool_matcher regex → warn + None, no panic ─────────

    #[test]
    fn set_webhooks_invalid_matcher_is_none_no_panic() {
        let mut reg = HookRegistry::new();
        let ssrf = crate::net::ssrf::ssrf_http_client(std::time::Duration::from_secs(5));
        reg.set_webhooks(ssrf, vec![
            crate::config::WebhookConfig {
                url: "https://x/h".into(),
                events: vec!["BeforeToolCall".into()],
                mode: crate::config::WebhookMode::Decision,
                tool_matcher: Some("[invalid(regex".into()),
                on_failure: crate::config::FailureMode::Open,
                timeout_ms: 3000,
                allow_internal: false,
            },
        ]);
        // Invalid regex → matcher compiled to None → first_matcher_matches returns false
        assert!(!reg.first_matcher_matches("anything"));
    }

    // ── Test 12 — http_client_internal reset on hot-reload without internal hooks ──

    #[test]
    fn set_webhooks_resets_internal_client_on_reload() {
        let mut reg = HookRegistry::new();
        let ssrf = crate::net::ssrf::ssrf_http_client(std::time::Duration::from_secs(5));
        // First call: allow_internal=true → internal client is built
        reg.set_webhooks(ssrf.clone(), vec![
            crate::config::WebhookConfig {
                url: "https://x/h".into(),
                events: vec!["BeforeToolCall".into()],
                mode: crate::config::WebhookMode::Decision,
                tool_matcher: None,
                on_failure: crate::config::FailureMode::Open,
                timeout_ms: 3000,
                allow_internal: true,
            },
        ]);
        assert!(reg.has_internal_client(), "internal client must be present after first call");
        // Second call (hot-reload): no internal hooks → stale client must be cleared
        reg.set_webhooks(ssrf, vec![
            crate::config::WebhookConfig {
                url: "https://x/h2".into(),
                events: vec!["BeforeToolCall".into()],
                mode: crate::config::WebhookMode::Async,
                tool_matcher: None,
                on_failure: crate::config::FailureMode::Open,
                timeout_ms: 3000,
                allow_internal: false,
            },
        ]);
        assert!(!reg.has_internal_client(), "stale internal client must be reset on hot-reload");
    }

    // ── Tests 13–18: fire_decision (WireMock) ───────────────────────────────

    #[tokio::test]
    async fn fire_decision_block_vetoes() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/h"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"decision":"block","reason":"nope"})))
            .mount(&server).await;
        let mut reg = HookRegistry::new();
        reg.set_webhooks(reqwest::Client::new(),
            vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
        let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "code_exec".into() };
        let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
        assert!(matches!(d, HookDecision::Block(r) if r == "nope"));
    }

    #[tokio::test]
    async fn fire_decision_modify_args() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/h"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"modified_args":{"x":2}})))
            .mount(&server).await;
        let mut reg = HookRegistry::new();
        reg.set_webhooks(reqwest::Client::new(),
            vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
        let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "code_exec".into() };
        let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{"x":1}})).await;
        match d {
            HookDecision::ModifyArgs(v) => assert_eq!(v["x"], 2),
            o => panic!("expected ModifyArgs, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn fire_decision_transform_result_has_provenance() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/h"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"transformed_result":"clean"})))
            .mount(&server).await;
        let mut reg = HookRegistry::new();
        reg.set_webhooks(reqwest::Client::new(),
            vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
        let ev = HookEvent::AfterToolResult { agent: "A".into(), tool_name: "t".into(), duration_ms: 1 };
        let d = reg.fire_decision(&ev, serde_json::json!({"result":"orig"})).await;
        match d {
            HookDecision::TransformResult(s) => assert!(s.starts_with("[hook:"), "missing provenance: {s}"),
            o => panic!("expected TransformResult, got {o:?}"),
        }
    }

    #[tokio::test]
    async fn fire_decision_matcher_skips_nonmatching_tool() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/h"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"decision":"block","reason":"x"})))
            .mount(&server).await;
        let mut reg = HookRegistry::new();
        reg.set_webhooks(reqwest::Client::new(),
            vec![decision_hook(format!("{}/h", server.uri()), Some("code_.*".into()), FailureMode::Open)]);
        let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "workspace_write".into() };
        let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
        assert!(matches!(d, HookDecision::Continue));
    }

    #[tokio::test]
    async fn fire_decision_failclosed_on_unreachable_blocks() {
        let mut reg = HookRegistry::new();
        // unroutable port → connect error
        reg.set_webhooks(reqwest::Client::new(),
            vec![decision_hook("http://127.0.0.1:1/h".into(), None, FailureMode::Closed)]);
        let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "t".into() };
        let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
        assert!(matches!(d, HookDecision::Block(_)));
    }

    #[tokio::test]
    async fn fire_decision_failopen_on_unreachable_continues() {
        let mut reg = HookRegistry::new();
        reg.set_webhooks(reqwest::Client::new(),
            vec![decision_hook("http://127.0.0.1:1/h".into(), None, FailureMode::Open)]);
        let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "t".into() };
        let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
        assert!(matches!(d, HookDecision::Continue));
    }

    // ── Test 20 — fire_decision Continue keeps original result unchanged ─────

    #[tokio::test]
    async fn fire_decision_continue_keeps_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/h"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server).await;
        let mut reg = HookRegistry::new();
        reg.set_webhooks(reqwest::Client::new(), vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
        let ev = HookEvent::AfterToolResult { agent: "A".into(), tool_name: "t".into(), duration_ms: 1 };
        let d = reg.fire_decision(&ev, serde_json::json!({"result":"orig"})).await;
        assert!(matches!(d, HookDecision::Continue));
    }

    // ── Test 21 — fire_decision BeforeMessage inject has provenance ─────────

    #[tokio::test]
    async fn fire_decision_inject_context_has_provenance() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/h"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"inject_context": "today is friday"})))
            .mount(&server).await;
        let mut reg = HookRegistry::new();
        reg.set_webhooks(
            reqwest::Client::new(),
            vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)],
        );
        let ev = HookEvent::BeforeMessage;
        let d = reg.fire_decision(&ev, serde_json::json!({"message": "hi"})).await;
        match d {
            HookDecision::InjectContext(s) => {
                assert!(s.starts_with("[hook:"), "missing provenance tag: {s}");
                assert!(s.contains("today is friday"), "content missing: {s}");
            }
            o => panic!("expected InjectContext, got {o:?}"),
        }
    }

    // ── Test 19 — fire_decision chains modified_args across hooks ────────────
    // hook1 sets x=2, hook2 sees x=2 in cur_extra and sets x=3.
    // Final decision must be ModifyArgs with x=3.
    #[tokio::test]
    async fn fire_decision_chains_modified_args() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/h1"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"modified_args": {"x": 2}})))
            .mount(&server).await;
        Mock::given(method("POST")).and(path("/h2"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"modified_args": {"x": 3}})))
            .mount(&server).await;
        let mut reg = HookRegistry::new();
        reg.set_webhooks(
            reqwest::Client::new(),
            vec![
                decision_hook(format!("{}/h1", server.uri()), None, FailureMode::Open),
                decision_hook(format!("{}/h2", server.uri()), None, FailureMode::Open),
            ],
        );
        let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "t".into() };
        let d = reg.fire_decision(&ev, serde_json::json!({"tool_input": {"x": 1}})).await;
        match d {
            HookDecision::ModifyArgs(v) => assert_eq!(v["x"], 3, "chain must deliver final x=3"),
            o => panic!("expected ModifyArgs, got {o:?}"),
        }
    }
}

#[cfg(test)]
mod chain_budget_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn budget_not_exceeded_below_limit() {
        assert!(!webhook_chain_exceeded(Duration::from_millis(9_000), 10_000));
    }
    #[test]
    fn budget_exceeded_at_or_above_limit() {
        assert!(webhook_chain_exceeded(Duration::from_millis(10_000), 10_000));
        assert!(webhook_chain_exceeded(Duration::from_millis(12_500), 10_000));
    }
    #[test]
    fn zero_budget_means_no_limit() {
        // 0 disables the chain budget entirely
        assert!(!webhook_chain_exceeded(Duration::from_secs(3_600), 0));
    }
}
