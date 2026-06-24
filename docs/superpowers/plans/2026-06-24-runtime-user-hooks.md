# Runtime User-Hooks (decision-webhooks) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Дать оператору определять хуки в рантайме (без пересборки) через синхронные decision-webhooks, способные ветировать/модифицировать/инжектить поток агента.

**Architecture:** Расширяем существующий `HookRegistry` (`agent/hooks.rs`): новый `HookDecision` + async `fire_decision(event, extra)`. `WebhookConfig` получает `mode` (async|decision), `tool_matcher`, `on_failure`, `timeout_ms`, `allow_internal`. Decision-webhooks POSTятся синхронно через SSRF-клиент (или plain при `allow_internal`), ответ парсится в решение. Три fire-точки: BeforeMessage (в bootstrap), BeforeToolCall, AfterToolResult.

**Tech Stack:** Rust 2024, `reqwest` (rustls), `regex` (уже в deps), `serde_json`, `tokio`, `wiremock 0.6.5` (dev-dep, для integration-тестов). Новых зависимостей НЕТ.

**Спека:** `docs/superpowers/specs/2026-06-24-runtime-user-hooks-design.md` (v2).

## Global Constraints

- **Работа напрямую в `master`**. НЕ создавать ветки. **NO git push** без явного разрешения. **NO `Co-Authored-By`** / упоминаний AI в коммитах.
- **TDD**: сначала падающий тест, потом реализация. Частые коммиты. (make НЕ установлен — прямые `cargo`.)
- **Backward-compat:** старые webhooks без `mode` → `Async` (текущее fire-and-forget поведение не меняется).
- **fire-точки decision** (sync `fire()` для closures остаётся, decision-webhook — отдельный async вызов ПОСЛЕ): BeforeMessage — внутри `bootstrap` (после `enriched_text`), BeforeToolCall — `engine_dispatch.rs:142`, AfterToolResult — `engine_dispatch.rs:49`.
- **Decision-возможности:** BeforeMessage→block|inject_context; BeforeToolCall→block|modified_args; AfterToolResult→transformed_result.
- **on_failure** (decision): timeout/ошибка/невалидный JSON → `Open` (default) = Continue (warn); `Closed` = Block («hook unavailable»). Для transform-событий closed неприменим → оригинал + warn.
- **Семантика per-event-instance:** несколько decision-webhooks на ОДНОМ событии — последовательно, first-Block short-circuit, modify/transform/inject чейнятся. Параллельные tool-calls — независимы.
- **SSRF:** decision-webhook через `ssrf_http_client` (default); `allow_internal=true` → plain `reqwest::Client` (для localhost/LAN hook).
- **Provenance + анти-spoof:** `inject_context`/`transformed_result` — санитайз вхождений `[hook:` затем префикс `[hook:{host}] `.
- **Audit:** новый `AuditEvent::HookDecision` (worker-арм через структурный tracing — без новой таблицы/миграции).
- **Timeout decision** default 3000ms, cap 30000; входит в бюджет tool-timeout.

## File Structure

- **Modify** `crates/opex-core/src/config/mod.rs` — `WebhookConfig` + `WebhookMode`/`FailureMode` enums.
- **Modify** `crates/opex-core/src/agent/hooks.rs` — `HookDecision`, `WebhookResponse`/`parse_decision`, `CompiledWebhook`, `set_webhooks` (compile+clients), `fire_decision`, sanitize/provenance helpers.
- **Modify** `crates/opex-core/src/db/audit_queue.rs` — `AuditEvent::HookDecision` variant + worker arm.
- **Modify** `crates/opex-core/src/agent/engine_dispatch.rs` — BeforeToolCall (block+modify), AfterToolResult (transform).
- **Modify** `crates/opex-core/src/agent/pipeline/bootstrap.rs` — BeforeMessage decision (block+inject).
- **Test** WireMock: `#[cfg(test)] mod tests` ВНУТРИ `crates/opex-core/src/agent/hooks.rs` (внутрикрейтовые — `lib.rs` НЕ экспонирует `agent::hooks`/`config` для внешнего крейта; dev-dep `wiremock 0.6.5` доступен unit-тестам). Внутренние типы — через `super::*` / `crate::config::*`. НЕ создавать `tests/integration_hooks.rs`.

---

### Task 1: `WebhookConfig` extension + enums

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (`WebhookConfig` ~902-913)
- Test: `#[cfg(test)]` в `config/mod.rs`

**Interfaces:**
- Produces: `WebhookMode {Async, Decision}`, `FailureMode {Open, Closed}` (serde lowercase, `Default`); `WebhookConfig` поля `mode/tool_matcher/on_failure/timeout_ms/allow_internal`.

- [ ] **Step 1: Падающий тест**

В `#[cfg(test)] mod tests` config/mod.rs:

```rust
#[test]
fn webhook_config_backward_compat_defaults_async() {
    let toml = r#"url = "https://x/h"
events = ["BeforeToolCall"]"#;
    let w: WebhookConfig = toml::from_str(toml).unwrap();
    assert!(matches!(w.mode, WebhookMode::Async));
    assert!(matches!(w.on_failure, FailureMode::Open));
    assert_eq!(w.timeout_ms, 3000);
    assert!(!w.allow_internal);
    assert!(w.tool_matcher.is_none());
}

#[test]
fn webhook_config_decision_parses() {
    let toml = r#"url = "https://x/h"
events = ["BeforeToolCall"]
mode = "decision"
tool_matcher = "code_exec|workspace_.*"
on_failure = "closed"
timeout_ms = 1500
allow_internal = true"#;
    let w: WebhookConfig = toml::from_str(toml).unwrap();
    assert!(matches!(w.mode, WebhookMode::Decision));
    assert!(matches!(w.on_failure, FailureMode::Closed));
    assert_eq!(w.timeout_ms, 1500);
    assert!(w.allow_internal);
    assert_eq!(w.tool_matcher.as_deref(), Some("code_exec|workspace_.*"));
}
```

- [ ] **Step 2: Запустить — FAIL**

Run: `cargo test --bin opex-core webhook_config -- --nocapture`
Expected: FAIL — `WebhookMode`/поля не найдены.

- [ ] **Step 3: Реализация**

В `config/mod.rs` рядом с `WebhookConfig` добавить enums:

```rust
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebhookMode {
    #[default]
    Async,
    Decision,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FailureMode {
    #[default]
    Open,
    Closed,
}

fn default_hook_timeout_ms() -> u64 { 3000 }
```

Расширить `WebhookConfig` (добавить поля после `events`):

```rust
    #[serde(default)]
    pub mode: WebhookMode,
    /// Regex on tool_name (BeforeToolCall/AfterToolResult). None = all tools.
    #[serde(default)]
    pub tool_matcher: Option<String>,
    #[serde(default)]
    pub on_failure: FailureMode,
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
    /// true → bypass SSRF resolver (admin opt-in for localhost/LAN hook service).
    #[serde(default)]
    pub allow_internal: bool,
```

- [ ] **Step 4: Запустить — PASS**

Run: `cargo test --bin opex-core webhook_config -- --nocapture`
Expected: PASS (2 теста).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/config/mod.rs
git commit -m "feat(hooks): WebhookConfig — mode/tool_matcher/on_failure/timeout_ms/allow_internal"
```

---

### Task 2: `HookDecision` + парсинг ответа webhook

**Files:**
- Modify: `crates/opex-core/src/agent/hooks.rs`
- Test: `#[cfg(test)]` в `hooks.rs`

**Interfaces:**
- Consumes: `HookEvent` (existing).
- Produces:
  - `pub enum HookDecision { Continue, Block(String), ModifyArgs(serde_json::Value), InjectContext(String), TransformResult(String) }`
  - `pub(crate) fn parse_decision(body: &str, event: &HookEvent) -> HookDecision`
  - `fn event_wire_name(event: &HookEvent) -> &'static str`
  - `fn event_tool_name(event: &HookEvent) -> Option<&str>`

- [ ] **Step 1: Падающий тест**

В `#[cfg(test)] mod tests` hooks.rs:

```rust
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

#[test]
fn event_wire_helpers() {
    let btc = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "tool".into() };
    assert_eq!(event_wire_name(&btc), "BeforeToolCall");
    assert_eq!(event_tool_name(&btc), Some("tool"));
    assert_eq!(event_tool_name(&HookEvent::BeforeMessage), None);
}
```

- [ ] **Step 2: FAIL**

Run: `cargo test --bin opex-core hooks::tests::parse_decision_variants hooks::tests::event_wire_helpers -- --nocapture`
Expected: FAIL — `HookDecision`/`parse_decision` не найдены.

- [ ] **Step 3: Реализация**

В `hooks.rs` добавить:

```rust
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

pub(crate) fn event_wire_name(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::BeforeMessage => "BeforeMessage",
        HookEvent::AfterResponse => "AfterResponse",
        HookEvent::BeforeToolCall { .. } => "BeforeToolCall",
        HookEvent::AfterToolResult { .. } => "AfterToolResult",
        HookEvent::OnError => "OnError",
    }
}

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
```

- [ ] **Step 4: PASS**

Run: `cargo test --bin opex-core hooks::tests::parse_decision_variants hooks::tests::event_wire_helpers -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/hooks.rs
git commit -m "feat(hooks): HookDecision + parse_decision + event wire helpers"
```

---

### Task 3: `CompiledWebhook` storage + `set_webhooks` (matcher + clients) + provenance/sanitize

**Files:**
- Modify: `crates/opex-core/src/agent/hooks.rs`
- Test: `#[cfg(test)]` в `hooks.rs`

**Interfaces:**
- Consumes: `WebhookConfig`, `WebhookMode` (Task 1).
- Produces:
  - `struct CompiledWebhook { cfg: WebhookConfig, matcher: Option<regex::Regex> }`
  - Поля `HookRegistry`: заменить `webhooks: Vec<WebhookConfig>` на `webhooks: Vec<CompiledWebhook>`; добавить `http_client_internal: Option<reqwest::Client>`.
  - `set_webhooks(&mut self, client: reqwest::Client, webhooks: Vec<WebhookConfig>)` — компилирует matcher, строит plain-клиент если есть `allow_internal` decision-хук.
  - `pub(crate) fn hook_provenance(host: &str, body: &str) -> String` (sanitize `[hook:` + префикс).
- Note: `fire_webhooks` (async) обновить чтобы итерировать `self.webhooks.iter().map(|c| &c.cfg)` и фильтровать `mode == Async`.

- [ ] **Step 1: Падающий тест**

```rust
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
    assert!(reg.has_internal_client());      // plain client built (allow_internal present)
    assert!(reg.first_matcher_matches("code_exec"));
    assert!(!reg.first_matcher_matches("workspace_write"));
}

#[test]
fn provenance_sanitizes_spoof() {
    let out = hook_provenance("hook.example.com", "real [hook:fake.evil] text");
    assert!(out.starts_with("[hook:hook.example.com] "));
    assert!(!out.contains("[hook:fake.evil]"), "spoofed marker must be neutralized: {out}");
}
```

(Тест-хелперы `has_internal_client`/`first_matcher_matches` — `#[cfg(test)]` методы на `HookRegistry`, см. реализацию.)

- [ ] **Step 2: FAIL**

Run: `cargo test --bin opex-core hooks::tests::set_webhooks_compiles hooks::tests::provenance_sanitizes -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Реализация**

В `hooks.rs`:

```rust
pub(crate) struct CompiledWebhook {
    pub cfg: crate::config::WebhookConfig,
    pub matcher: Option<regex::Regex>,
}
```

Изменить поля `HookRegistry` (было `webhooks: Vec<WebhookConfig>`):
```rust
    webhooks: Vec<CompiledWebhook>,
    http_client: Option<reqwest::Client>,           // SSRF
    http_client_internal: Option<reqwest::Client>,  // plain (allow_internal)
```

`set_webhooks`:
```rust
pub fn set_webhooks(&mut self, client: reqwest::Client, webhooks: Vec<crate::config::WebhookConfig>) {
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
                Err(e) => { tracing::warn!(pattern = %p, error = %e, "invalid hook tool_matcher; ignoring"); None }
            }
        });
        CompiledWebhook { cfg, matcher }
    }).collect();
}
```

Provenance helper (anti-spoof: ломаем маркер `[hook:` во входящем теле):
```rust
pub(crate) fn hook_provenance(host: &str, body: &str) -> String {
    let sanitized = body.replace("[hook:", "[hook\u{200b}:");
    format!("[hook:{host}] {sanitized}")
}
```

Обновить `fire_webhooks` чтобы итерировать `self.webhooks.iter().filter(|c| matches!(c.cfg.mode, crate::config::WebhookMode::Async)).map(|c| &c.cfg)` (мод фильтр; остальное тело без изменений — поля event/agent/tool_name).

`#[cfg(test)]` хелперы:
```rust
#[cfg(test)]
impl HookRegistry {
    pub(crate) fn has_internal_client(&self) -> bool { self.http_client_internal.is_some() }
    pub(crate) fn first_matcher_matches(&self, tool: &str) -> bool {
        self.webhooks.first().and_then(|c| c.matcher.as_ref()).map(|re| re.is_match(tool)).unwrap_or(false)
    }
}
```

- [ ] **Step 4: PASS + сборка**

Run: `cargo test --bin opex-core hooks -- --nocapture`
Expected: PASS (вкл. Task 2 тесты).
Run: `cargo check --all-targets`
Expected: PASS (fire_webhooks обновлён под новый тип поля).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/hooks.rs
git commit -m "feat(hooks): CompiledWebhook storage, set_webhooks matcher+internal client, provenance sanitize"
```

---

### Task 4: `fire_decision` core + WireMock integration tests

**Files:**
- Modify: `crates/opex-core/src/agent/hooks.rs`
- Test: `#[cfg(test)] mod tests` в `crates/opex-core/src/agent/hooks.rs` (WireMock, внутрикрейтовые)

**Interfaces:**
- Consumes: `parse_decision`, `event_wire_name`, `event_tool_name`, `hook_provenance`, `CompiledWebhook`, clients (Task 2-3).
- Produces: `pub async fn fire_decision(&self, event: &HookEvent, extra: serde_json::Value) -> HookDecision`.

- [ ] **Step 1: Падающий integration-тест (WireMock)**

Добавить WireMock-тесты в `#[cfg(test)] mod tests` внутри `hooks.rs` (внутрикрейтовые; типы через `super`). Импорты в начале тест-модуля:

```rust
use super::*;  // HookRegistry, HookEvent, HookDecision, fire_decision
use crate::config::{WebhookConfig, WebhookMode, FailureMode};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wiremock::matchers::{method, path};

fn decision_hook(url: String, matcher: Option<String>, on_failure: FailureMode) -> WebhookConfig {
    WebhookConfig {
        url, events: vec!["BeforeToolCall".into(), "AfterToolResult".into(), "BeforeMessage".into()],
        mode: WebhookMode::Decision, tool_matcher: matcher, on_failure,
        timeout_ms: 3000, allow_internal: true, // localhost wiremock → bypass SSRF
    }
}

#[tokio::test]
async fn fire_decision_block_vetoes() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/h"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"decision":"block","reason":"nope"})))
        .mount(&server).await;
    let mut reg = HookRegistry::new();
    reg.set_webhooks(reqwest::Client::new(), vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
    let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "code_exec".into() };
    let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
    assert!(matches!(d, HookDecision::Block(r) if r == "nope"));
}

#[tokio::test]
async fn fire_decision_modify_args() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/h"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"modified_args":{"x":2}})))
        .mount(&server).await;
    let mut reg = HookRegistry::new();
    reg.set_webhooks(reqwest::Client::new(), vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
    let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "code_exec".into() };
    let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{"x":1}})).await;
    match d { HookDecision::ModifyArgs(v) => assert_eq!(v["x"], 2), o => panic!("{o:?}") }
}

#[tokio::test]
async fn fire_decision_transform_result_has_provenance() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/h"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"transformed_result":"clean"})))
        .mount(&server).await;
    let mut reg = HookRegistry::new();
    reg.set_webhooks(reqwest::Client::new(), vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
    let ev = HookEvent::AfterToolResult { agent: "A".into(), tool_name: "t".into(), duration_ms: 1 };
    let d = reg.fire_decision(&ev, serde_json::json!({"result":"orig"})).await;
    match d { HookDecision::TransformResult(s) => assert!(s.starts_with("[hook:")), o => panic!("{o:?}") }
}

#[tokio::test]
async fn fire_decision_matcher_skips_nonmatching_tool() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/h"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"decision":"block","reason":"x"})))
        .mount(&server).await;
    let mut reg = HookRegistry::new();
    reg.set_webhooks(reqwest::Client::new(), vec![decision_hook(format!("{}/h", server.uri()), Some("code_.*".into()), FailureMode::Open)]);
    let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "workspace_write".into() };
    let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
    assert!(matches!(d, HookDecision::Continue));
}

#[tokio::test]
async fn fire_decision_failclosed_on_unreachable_blocks() {
    let mut reg = HookRegistry::new();
    // unroutable URL → connect error
    reg.set_webhooks(reqwest::Client::new(), vec![decision_hook("http://127.0.0.1:1/h".into(), None, FailureMode::Closed)]);
    let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "t".into() };
    let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
    assert!(matches!(d, HookDecision::Block(_)));
}

#[tokio::test]
async fn fire_decision_failopen_on_unreachable_continues() {
    let mut reg = HookRegistry::new();
    reg.set_webhooks(reqwest::Client::new(), vec![decision_hook("http://127.0.0.1:1/h".into(), None, FailureMode::Open)]);
    let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "t".into() };
    let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{}})).await;
    assert!(matches!(d, HookDecision::Continue));
}
```

> Тесты живут в `#[cfg(test)] mod tests` ВНУТРИ `hooks.rs` (НЕ внешний `tests/`-крейт — `lib.rs` не экспонирует `agent::hooks`/`config`). Внутри модуля всё достижимо через `super::*`/`crate::config::*` без `pub`-расширения. `decision_hook`-хелпер — в том же тест-модуле (используется и в Task 6/7/8). `wiremock` (dev-dep) доступен unit-тестам.

- [ ] **Step 2: FAIL**

Run: `cargo test --bin opex-core -- --nocapture`
Expected: FAIL — `fire_decision` не найден.

- [ ] **Step 3: Реализация `fire_decision`**

В `hooks.rs`:

```rust
impl HookRegistry {
    /// Run synchronous decision-webhooks for `event`. `extra` carries event-specific
    /// data: `{"tool_input": <args>}` (BeforeToolCall), `{"result": <str>}`
    /// (AfterToolResult), `{"message": <str>}` (BeforeMessage). Webhooks matching the
    /// event (and tool_matcher) run sequentially: first Block short-circuits;
    /// modified_args / transformed_result / inject_context chain across hooks.
    pub async fn fire_decision(&self, event: &HookEvent, extra: serde_json::Value) -> HookDecision {
        let ev_name = event_wire_name(event);
        let tool = event_tool_name(event);

        let mut cur_extra = extra;
        let mut modified_args: Option<serde_json::Value> = None;
        let mut transformed: Option<String> = None;
        let mut injected: Vec<String> = Vec::new();

        for cw in self.webhooks.iter()
            .filter(|c| matches!(c.cfg.mode, crate::config::WebhookMode::Decision))
        {
            if !cw.cfg.events.iter().any(|e| e == ev_name) { continue; }
            if let (Some(re), Some(t)) = (&cw.matcher, tool) {
                if !re.is_match(t) { continue; }
            }
            let client = if cw.cfg.allow_internal {
                self.http_client_internal.as_ref()
            } else {
                self.http_client.as_ref()
            };
            let Some(client) = client else { continue; };

            // Build request: event fields + current extra.
            let mut req = serde_json::json!({
                "event": ev_name,
                "agent": match event {
                    HookEvent::BeforeToolCall { agent, .. } | HookEvent::AfterToolResult { agent, .. } => agent.clone(),
                    _ => String::new(),
                },
            });
            if let Some(t) = tool { req["tool_name"] = serde_json::json!(t); }
            if let Some(obj) = req.as_object_mut() {
                if let Some(ex) = cur_extra.as_object() {
                    for (k, v) in ex { obj.insert(k.clone(), v.clone()); }
                }
            }

            let host = reqwest::Url::parse(&cw.cfg.url).ok()
                .and_then(|u| u.host_str().map(|s| s.to_string()))
                .unwrap_or_default();

            let fut = client.post(&cw.cfg.url).json(&req).send();
            let resp = tokio::time::timeout(
                std::time::Duration::from_millis(cw.cfg.timeout_ms.min(30_000)), fut,
            ).await;

            let body = match resp {
                Ok(Ok(r)) => r.text().await.unwrap_or_default(),
                _ => {
                    // timeout or transport error
                    tracing::warn!(url = %cw.cfg.url, "decision hook failed");
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
                    cur_extra["result"] = serde_json::json!(tagged);
                    transformed = Some(tagged);
                }
                HookDecision::InjectContext(s) => injected.push(hook_provenance(&host, &s)),
                HookDecision::Continue => {}
            }
        }

        if let Some(v) = modified_args { return HookDecision::ModifyArgs(v); }
        if let Some(s) = transformed { return HookDecision::TransformResult(s); }
        if !injected.is_empty() { return HookDecision::InjectContext(injected.join("\n")); }
        HookDecision::Continue
    }
}
```

- [ ] **Step 4: PASS**

Run: `cargo test --bin opex-core -- --nocapture`
Expected: PASS (6 тестов).
Run: `cargo check --all-targets`
Expected: PASS (`fire_decision` пока не вызван из prod → dead_code warning ОЖИДАЕМ; полный clippy — Task 6+).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/hooks.rs
git commit -m "feat(hooks): fire_decision (sequential, first-block, chaining, on_failure) + WireMock tests"
```

---

### Task 5: `AuditEvent::HookDecision` variant + worker arm

**Files:**
- Modify: `crates/opex-core/src/db/audit_queue.rs`
- Test: `#[cfg(test)]` в `audit_queue.rs`

**Interfaces:**
- Produces: `AuditEvent::HookDecision { agent_name, session_id: Option<Uuid>, event_type: String, action: String, detail: Option<String> }`; worker arm пишет структурный tracing-лог (без новой таблицы).

- [ ] **Step 1: Падающий тест**

```rust
#[tokio::test]
async fn hook_decision_event_constructs_and_sends() {
    // Lazy pool (never connects) — worker arm for HookDecision must not touch DB.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://x:x@127.0.0.1:1/x").unwrap();
    let q = AuditQueue::new(pool);
    q.send(AuditEvent::HookDecision {
        agent_name: "A".into(),
        session_id: None,
        event_type: "BeforeToolCall".into(),
        action: "Block".into(),
        detail: Some("reason".into()),
    });
    // No panic; give worker a tick.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
```

- [ ] **Step 2: FAIL**

Run: `cargo test --bin opex-core hook_decision_event_constructs -- --nocapture`
Expected: FAIL — вариант не найден.

- [ ] **Step 3: Реализация**

В `audit_queue.rs` добавить вариант в `AuditEvent`:
```rust
    HookDecision {
        agent_name: String,
        session_id: Option<Uuid>,
        event_type: String,
        action: String,           // "Block" | "ModifyArgs" | "InjectContext" | "TransformResult"
        detail: Option<String>,   // truncated reason/diff ≤512B
    },
```

В worker `match event { ... }` добавить арм (tracing-backed audit — без новой таблицы/миграции):
```rust
        AuditEvent::HookDecision { agent_name, session_id, event_type, action, detail } => {
            tracing::info!(
                target: "hook_audit",
                agent = %agent_name,
                session = ?session_id,
                event = %event_type,
                action = %action,
                detail = detail.as_deref().unwrap_or(""),
                "hook decision",
            );
        }
```

- [ ] **Step 4: PASS**

Run: `cargo test --bin opex-core hook_decision_event_constructs -- --nocapture`
Expected: PASS.
Run: `cargo check --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/db/audit_queue.rs
git commit -m "feat(hooks): AuditEvent::HookDecision variant + tracing-backed worker arm"
```

---

### Task 6: BeforeToolCall integration (block + modify-args)

**Files:**
- Modify: `crates/opex-core/src/agent/engine_dispatch.rs` (~142-150)
- Test: добавить в `#[cfg(test)] mod tests` в `crates/opex-core/src/agent/hooks.rs`

**Interfaces:**
- Consumes: `fire_decision` (Task 4), `AuditEvent::HookDecision` (Task 5), rebind-паттерн `ApprovedWithModifiedArgs` (engine_dispatch.rs:121-128).

- [ ] **Step 1: Реализация в fire-точке BeforeToolCall**

В `engine_dispatch.rs`, после существующего sync-блока BeforeToolCall (после `if let HookAction::Block(reason) = action { return ... }`), добавить async decision:

```rust
        // Decision-webhooks (async): block veto or modified args.
        let decision = self.hooks().fire_decision(
            &hook_event,
            serde_json::json!({ "tool_input": arguments }),
        ).await;
        match decision {
            crate::agent::hooks::HookDecision::Block(reason) => {
                self.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                    agent_name: self.cfg().agent.name.clone(),
                    session_id: None,
                    event_type: "BeforeToolCall".into(),
                    action: "Block".into(),
                    detail: Some(reason.chars().take(512).collect()),
                });
                return format!("Tool blocked by hook: {}", reason);
            }
            crate::agent::hooks::HookDecision::ModifyArgs(mut modified) => {
                // Preserve internal _context (mirror ApprovedWithModifiedArgs rebind).
                if let Some(ctx) = arguments.get("_context")
                    && let Some(obj) = modified.as_object_mut()
                {
                    obj.insert("_context".to_string(), ctx.clone());
                }
                self.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                    agent_name: self.cfg().agent.name.clone(),
                    session_id: None,
                    event_type: "BeforeToolCall".into(),
                    action: "ModifyArgs".into(),
                    detail: None,
                });
                return self.execute_tool_call(name, &modified).await;
            }
            _ => {}
        }
```

> Размещение: ПОСЛЕ sync block-проверки, ДО фактического исполнения инструмента (там же, где раньше шёл вызов исполнения). `arguments`/`name` доступны в `execute_tool_call_inner` (engine_dispatch.rs:83). Если decision-точка во `_inner`, `execute_tool_call(name, &modified)` повторно прогонит approval+hooks для НОВЫХ args — приемлемо (как делает approval rebind:127). Чтобы избежать рекурсии хука на модифицированных args, modify-rebind допустимо вызывать через `execute_tool_call_inner` напрямую (без повторного hook); реализатор выбирает согласованно с approval-паттерном и фиксирует в отчёте.

- [ ] **Step 2: Integration-тест (WireMock + реальный engine — ИЛИ unit на decision-маппинг)**

Полный engine-тест тяжёл (нужен LLM/DB). Минимально достаточно: integration-тест `fire_decision` уже покрывает Block/ModifyArgs (Task 4). Добавить в `integration_hooks.rs` тест чейнинга (rebind-семантика проверяется на уровне fire_decision):

```rust
#[tokio::test]
async fn fire_decision_chains_modified_args() {
    let server = MockServer::start().await;
    // hook1 sets x=2, hook2 sees x=2 and sets x=3
    Mock::given(method("POST")).and(path("/h1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"modified_args":{"x":2}})))
        .mount(&server).await;
    Mock::given(method("POST")).and(path("/h2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"modified_args":{"x":3}})))
        .mount(&server).await;
    let mut reg = HookRegistry::new();
    reg.set_webhooks(reqwest::Client::new(), vec![
        decision_hook(format!("{}/h1", server.uri()), None, FailureMode::Open),
        decision_hook(format!("{}/h2", server.uri()), None, FailureMode::Open),
    ]);
    let ev = HookEvent::BeforeToolCall { agent: "A".into(), tool_name: "t".into() };
    let d = reg.fire_decision(&ev, serde_json::json!({"tool_input":{"x":1}})).await;
    match d { HookDecision::ModifyArgs(v) => assert_eq!(v["x"], 3), o => panic!("{o:?}") }
}
```

- [ ] **Step 3: Запустить**

Run: `cargo test --bin opex-core fire_decision_chains -- --nocapture`
Expected: PASS.
Run: `cargo check --all-targets`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/engine_dispatch.rs crates/opex-core/src/agent/hooks.rs
git commit -m "feat(hooks): BeforeToolCall decision — block veto + modified-args rebind + audit"
```

---

### Task 7: AfterToolResult integration (transform)

**Files:**
- Modify: `crates/opex-core/src/agent/engine_dispatch.rs` (~49-56)

**Interfaces:**
- Consumes: `fire_decision` (Task 4), `AuditEvent::HookDecision`. Точка: outer `execute_tool_call` где доступен `result: String` + `duration_ms`.

- [ ] **Step 1: Реализация**

В `engine_dispatch.rs` AfterToolResult-точке (там, где есть итоговый `result` строкой, после sync `fire`/`fire_webhooks`), заменить пассивный fire на decision-обработку:

```rust
        let decision = self.hooks().fire_decision(
            &hook_event,
            serde_json::json!({ "result": result }),
        ).await;
        let result = if let crate::agent::hooks::HookDecision::TransformResult(s) = decision {
            self.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                agent_name: self.cfg().agent.name.clone(),
                session_id: None,
                event_type: "AfterToolResult".into(),
                action: "TransformResult".into(),
                detail: None,
            });
            s
        } else {
            result
        };
```

> `result` — итоговая строка результата инструмента в outer `execute_tool_call` (где формируется `duration_ms`). `let result = ...` shadowing заменяет значение перед возвратом/добавлением в контекст. Реализатор размещает блок там, где `result` ещё мутабелен/возвращается, и фиксирует точную строку в отчёте.

- [ ] **Step 2: Тест**

Покрытие transform + provenance — integration-тест `fire_decision_transform_result_has_provenance` (Task 4). Дополнительно проверить, что для НЕ-transform решения результат не меняется — добавить в `#[cfg(test)] mod tests` в `hooks.rs`:

```rust
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
```

- [ ] **Step 3: Запустить**

Run: `cargo test --bin opex-core fire_decision_continue_keeps -- --nocapture`
Expected: PASS.
Run: `cargo check --all-targets`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/engine_dispatch.rs crates/opex-core/src/agent/hooks.rs
git commit -m "feat(hooks): AfterToolResult decision — transform result + provenance + audit"
```

---

### Task 8: BeforeMessage integration (block + inject) в bootstrap

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/bootstrap.rs` (после `enriched_text` ~260)

**Interfaces:**
- Consumes: `fire_decision` (Task 4), `AuditEvent::HookDecision`. `engine.hooks()` доступен в `bootstrap`.

- [ ] **Step 1: Реализация**

В `bootstrap.rs`, после построения `enriched_text` (строка ~260), до персиста user-message (~289):

```rust
    // Decision-webhooks for BeforeMessage: block the turn or inject context.
    let bm_event = crate::agent::hooks::HookEvent::BeforeMessage;
    let bm_decision = engine.hooks().fire_decision(
        &bm_event,
        serde_json::json!({ "message": enriched_text }),
    ).await;
    let enriched_text = match bm_decision {
        crate::agent::hooks::HookDecision::Block(reason) => {
            engine.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                agent_name: engine.cfg().agent.name.clone(),
                session_id: Some(session_id),
                event_type: "BeforeMessage".into(),
                action: "Block".into(),
                detail: Some(reason.chars().take(512).collect()),
            });
            anyhow::bail!("blocked by hook: {}", reason);
        }
        crate::agent::hooks::HookDecision::InjectContext(ctx) => {
            engine.cfg().audit_queue.send(crate::db::audit_queue::AuditEvent::HookDecision {
                agent_name: engine.cfg().agent.name.clone(),
                session_id: Some(session_id),
                event_type: "BeforeMessage".into(),
                action: "InjectContext".into(),
                detail: None,
            });
            // Inject hook context ahead of the user message (provenance already tagged).
            format!("{ctx}\n\n{enriched_text}")
        }
        _ => enriched_text,
    };
```

> `session_id` доступен в bootstrap к этому моменту (claim прошёл). `enriched_text` shadowing вставляет инжект перед текстом пользователя. Реализатор сверяет, что `enriched_text` ещё используется ниже для персиста/контекста, и размещает блок до первого такого использования.

- [ ] **Step 2: Тест**

BeforeMessage inject покрывается unit-уровнем `fire_decision` — добавить в `#[cfg(test)] mod tests` в `hooks.rs`:

```rust
#[tokio::test]
async fn fire_decision_inject_context_has_provenance() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/h"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"inject_context":"today is friday"})))
        .mount(&server).await;
    let mut reg = HookRegistry::new();
    reg.set_webhooks(reqwest::Client::new(), vec![decision_hook(format!("{}/h", server.uri()), None, FailureMode::Open)]);
    let ev = HookEvent::BeforeMessage;
    let d = reg.fire_decision(&ev, serde_json::json!({"message":"hi"})).await;
    match d { HookDecision::InjectContext(s) => {
        assert!(s.starts_with("[hook:")); assert!(s.contains("today is friday"));
    }, o => panic!("{o:?}") }
}
```

- [ ] **Step 3: Запустить**

Run: `cargo test --bin opex-core fire_decision_inject -- --nocapture`
Expected: PASS.
Run: `cargo check --all-targets`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/bootstrap.rs crates/opex-core/src/agent/hooks.rs
git commit -m "feat(hooks): BeforeMessage decision in bootstrap — block + inject context + audit"
```

---

### Task 9: Конфиг-доки + финальный clippy-гейт

**Files:**
- Modify: `config/opex.toml` (закомментированный пример)
- Verify: весь проект

**Interfaces:** —

- [ ] **Step 1: Документация конфига**

В `config/opex.toml` добавить закомментированный пример decision-webhook (рядом с прочими секциями):

```toml
# Per-agent runtime hooks (в config/agents/{Name}.toml под [agent.hooks]):
# [[agent.hooks.webhooks]]
# url = "http://hooks.local:9000/gate"   # admin-hosted hook service
# events = ["BeforeToolCall"]            # BeforeMessage|BeforeToolCall|AfterToolResult
# mode = "decision"                       # "async" (fire-and-forget, default) | "decision" (sync, parse response)
# tool_matcher = "code_exec|workspace_.*" # regex on tool_name (decision tool-events)
# on_failure = "open"                     # "open" (continue, default) | "closed" (block on hook failure)
# timeout_ms = 3000                       # sync decision timeout (cap 30000)
# allow_internal = false                  # true → bypass SSRF (localhost/LAN hook service)
# Decision response JSON: {"decision":"block","reason":"..."} | {"modified_args":{...}}
#   | {"inject_context":"..."} | {"transformed_result":"..."} | {} (continue)
```

- [ ] **Step 2: Финальный гейт**

Run: `cargo test --bin opex-core hooks -- --nocapture` && `cargo test --bin opex-core -- --nocapture`
Expected: PASS (все hook-тесты).
Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS — весь decision-функционал теперь используется из prod (BeforeToolCall/AfterToolResult/BeforeMessage). Если остался dead_code/clippy — устранить (мёртвый код удалить или проверить путь; не глушить `#[allow]` без причины).

- [ ] **Step 3: Commit**

```bash
git add config/opex.toml
git commit -m "docs(hooks): пример decision-webhook конфига в opex.toml"
```

---

## Self-Review

**1. Spec coverage:**
- §1 механизм (decision-webhooks) → Task 4 fire_decision. ✓
- §2 возможности (block/inject/modify/transform) → Task 2 (parse) + 6/7/8 (fire-точки). ✓
- §3 modify/transform injection-vector → provenance/sanitize Task 3 + audit Task 5. ✓
- §4 on_failure → Task 4. ✓
- Конфиг (mode/matcher/on_failure/timeout/allow_internal) → Task 1. ✓
- HookDecision → Task 2. ✓
- fire_decision (sequential/first-block/chaining) → Task 4. ✓
- 3 fire-точки → Task 6 (BeforeToolCall), 7 (AfterToolResult), 8 (BeforeMessage/bootstrap). ✓
- AuditEvent::HookDecision → Task 5. ✓
- SSRF allow_internal → Task 3 (clients) + Task 4 (selection). ✓
- tool_matcher compile → Task 3 (set_webhooks). ✓
- provenance + анти-spoof → Task 3 (hook_provenance) + 7/8 (применение). ✓
- backward-compat (async default) → Task 1. ✓
- per-instance/parallel семантика → Task 4 (sequential within fire_decision; parallel tools independent by construction). ✓

**2. Placeholder scan:** код полный во всех шагах. Tasks 6/7/8 содержат примечания «реализатор сверяет точную строку» для shadowing-точек — это разметка места вставки в существующий поток, не плейсхолдер (код блока дан полностью).

**3. Type consistency:** `HookDecision {Continue,Block,ModifyArgs,InjectContext,TransformResult}` — консистентен Task 2/4/6/7/8. `fire_decision(&self, &HookEvent, serde_json::Value) -> HookDecision` — Task 4, вызовы 6/7/8 совпадают. `AuditEvent::HookDecision {agent_name,session_id,event_type,action,detail}` — Task 5, вызовы 6/7/8 совпадают. `WebhookConfig` поля — Task 1, использование Task 3/4. `hook_provenance(host,body)` — Task 3, использование Task 4. ✓
