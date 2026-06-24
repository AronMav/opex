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
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            http_client: None,
            http_client_internal: None,
            webhooks: Vec::new(),
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
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok();
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

    /// Fire matching async webhooks for `event`. Always returns immediately; the
    /// HTTP POST is dispatched on a detached `tokio::spawn` task with a
    /// 5-second timeout. Errors are logged at warn level and dropped — they
    /// NEVER alter HookAction (the existing `fire()` already returned for
    /// the synchronous policy decision).
    ///
    /// Only webhooks with `mode == Async` are dispatched here. Decision-mode
    /// webhooks are handled separately by the decision gateway (Task 4+).
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
    if let Some(args) = r.modified_args {
        if matches!(event, HookEvent::BeforeToolCall { .. }) && args.is_object() {
            return HookDecision::ModifyArgs(args);
        }
    }
    if let Some(res) = r.transformed_result {
        if matches!(event, HookEvent::AfterToolResult { .. }) {
            return HookDecision::TransformResult(res);
        }
    }
    if let Some(ctx) = r.inject_context {
        if matches!(event, HookEvent::BeforeMessage) {
            return HookDecision::InjectContext(ctx);
        }
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
    use crate::config::{HooksConfig, WebhookConfig};
    use std::time::Duration;

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
}
