# Plan A — Decision-hooks UI + data-loss фикс

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Редактировать decision-webhooks агента в UI (AgentEditDialog) + устранить data-loss (PUT агента сейчас обнуляет webhooks с диска).

**Architecture:** Backend — `WebhookDto` (ts-rs) в GET-DTO агента + preserve-on-omit webhooks в update-handler. UI — редактор webhook-строк в секции hooks AgentEditDialog, всегда шлёт полный массив.

**Tech Stack:** Rust (axum, serde, ts-rs за feature `ts-gen`), Next.js 16/React 19, shadcn (Switch/Input/Select), vitest.

**Спека:** `docs/superpowers/specs/2026-06-25-ui-hooks-checkpoint-design.md` (v2), План A.

## Global Constraints

- **master**; без push; без `Co-Authored-By`; TDD. (make нет — прямые cargo; `cd ui` для npm/vitest.)
- **CRIT data-loss:** read (GET-DTO webhooks) + write (preserve-on-omit + UI всегда шлёт webhooks) — закрыть в этом плане целиком.
- `WebhookDto` — НОВЫЙ ts-rs тип (НЕ reuse `WebhookConfig` — он не ts-rs). Поля: `url:String, events:Vec<String>, mode:String, tool_matcher:Option<String>, on_failure:String, timeout_ms:u64, allow_internal:bool`. `mode`/`on_failure` — lowercase ("async"|"decision", "open"|"closed").
- ts-rs codegen: `cargo run --bin gen_ts_types` (feature `ts-gen`); min-count guard `ui=34` в gen_ts_types.rs:50 поднять на +1 за каждый новый export.
- shadcn: `Checkbox` отсутствует → events через Switch/кнопки-тоглы или нативные checkbox.

## File Structure

- Modify `crates/opex-core/src/gateway/handlers/agents/dto_structs.rs` — `WebhookDto` + `AgentDetailHooksDto.webhooks`.
- Modify `crates/opex-core/src/gateway/handlers/agents/dto.rs` — заполнение webhooks в `from_config`.
- Modify `crates/opex-core/src/bin/gen_ts_types.rs` — register `WebhookDto` + min-count.
- Modify `crates/opex-core/src/gateway/handlers/agents/crud.rs` — preserve-on-omit webhooks.
- Modify `ui/src/types/api.generated.ts` — регенерируется codegen (не вручную).
- Modify `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` — webhooks-редактор.
- Modify `ui/src/app/(authenticated)/agents/page.tsx` — `formToPayload` шлёт webhooks.
- Test: dto serde (Rust), crud preserve (Rust), AgentEditDialog (vitest).

---

### Task 1: Backend — WebhookDto + AgentDetailHooksDto.webhooks + codegen

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/dto_structs.rs` (~150-157)
- Modify: `crates/opex-core/src/gateway/handlers/agents/dto.rs` (~126-129)
- Modify: `crates/opex-core/src/bin/gen_ts_types.rs` (~50)
- Test: `#[cfg(test)]` в dto_structs.rs или dto.rs

**Interfaces:**
- Produces: `WebhookDto { url, events, mode, tool_matcher, on_failure, timeout_ms, allow_internal }`; `AgentDetailHooksDto.webhooks: Vec<WebhookDto>`.

- [ ] **Step 1: Падающий тест — DTO содержит webhooks**

В `#[cfg(test)]` (dto_structs.rs):

```rust
#[test]
fn hooks_dto_serializes_webhooks() {
    let dto = AgentDetailHooksDto {
        log_all_tool_calls: false,
        block_tools: vec![],
        webhooks: vec![WebhookDto {
            url: "https://x/h".into(),
            events: vec!["BeforeToolCall".into()],
            mode: "decision".into(),
            tool_matcher: Some("code_.*".into()),
            on_failure: "closed".into(),
            timeout_ms: 1500,
            allow_internal: true,
        }],
    };
    let j = serde_json::to_value(&dto).unwrap();
    assert_eq!(j["webhooks"][0]["mode"], "decision");
    assert_eq!(j["webhooks"][0]["on_failure"], "closed");
    assert_eq!(j["webhooks"][0]["timeout_ms"], 1500);
}
```

- [ ] **Step 2: FAIL**

Run: `cargo test --bin opex-core hooks_dto_serializes_webhooks -- --nocapture`
Expected: FAIL — `WebhookDto`/поле `webhooks` не найдены.

- [ ] **Step 3: Реализация**

В `dto_structs.rs` рядом с `AgentDetailHooksDto`:

```rust
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct WebhookDto {
    pub url: String,
    pub events: Vec<String>,
    pub mode: String,           // "async" | "decision"
    pub tool_matcher: Option<String>,
    pub on_failure: String,     // "open" | "closed"
    pub timeout_ms: u64,
    pub allow_internal: bool,
}
crate::register_ts_dto!(WebhookDto);
```

Добавить поле в `AgentDetailHooksDto`:
```rust
    pub webhooks: Vec<WebhookDto>,
```

В `dto.rs` `from_config` (~126-129) заполнить webhooks из `h.webhooks` (мапинг enum→lowercase). `WebhookMode`/`FailureMode` сериализуются lowercase — используй `serde_json::to_value(&w.mode).unwrap().as_str()` ИЛИ явный match. Явный match надёжнее:

```rust
hooks: a.hooks.as_ref().map(|h| AgentDetailHooksDto {
    log_all_tool_calls: h.log_all_tool_calls,
    block_tools: h.block_tools.clone(),
    webhooks: h.webhooks.iter().map(|w| WebhookDto {
        url: w.url.clone(),
        events: w.events.clone(),
        mode: match w.mode { crate::config::WebhookMode::Async => "async", crate::config::WebhookMode::Decision => "decision" }.to_string(),
        tool_matcher: w.tool_matcher.clone(),
        on_failure: match w.on_failure { crate::config::FailureMode::Open => "open", crate::config::FailureMode::Closed => "closed" }.to_string(),
        timeout_ms: w.timeout_ms,
        allow_internal: w.allow_internal,
    }).collect(),
}),
```

В `gen_ts_types.rs` (~50): поднять `ui` min-count `34 → 35` (один новый export `WebhookDto`). (`register_ts_dto!` + inventory регистрируют автоматически.)

- [ ] **Step 4: PASS + codegen**

Run: `cargo test --bin opex-core hooks_dto_serializes_webhooks -- --nocapture` → PASS.
Run: `cargo run --bin gen_ts_types` → обновляет `ui/src/types/api.generated.ts` (AgentDetailHooksDto.webhooks + type WebhookDto). Убедись, что файл изменился и содержит `WebhookDto`.
Run: `cargo check --all-targets` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/agents/dto_structs.rs crates/opex-core/src/gateway/handlers/agents/dto.rs crates/opex-core/src/bin/gen_ts_types.rs ui/src/types/api.generated.ts
git commit -m "feat(ui-hooks): WebhookDto + webhooks в AgentDetailHooksDto (GET-DTO) + codegen"
```

---

### Task 2: Backend — preserve-on-omit webhooks (data-loss фикс)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/agents/crud.rs` (preserve-блок ~690-701)
- Test: `#[cfg(test)]` в crud.rs (или интеграционный, если есть харнесс)

**Interfaces:**
- Consumes: `AgentDetailHooksDto.webhooks` (A1, для round-trip), `HooksConfig.webhooks`.

- [ ] **Step 1: Падающий тест — PUT без webhooks сохраняет существующие**

> crud-хендлер тяжёл для unit-теста (нужен AppState/DB). Если в crud.rs нет лёгкого теста — извлеки preserve-логику в чистую функцию `preserve_hooks_webhooks(new: &mut AgentConfig, existing: &AgentConfig, payload_webhooks_present: bool)` и протестируй ЕЁ:

```rust
#[test]
fn preserve_webhooks_when_payload_omits() {
    use crate::config::{HooksConfig, WebhookConfig};
    let mut new_cfg = test_agent_config(); // helper; hooks.webhooks = []
    new_cfg.agent.hooks = Some(HooksConfig { log_all_tool_calls: true, block_tools: vec![], webhooks: vec![] });
    let mut existing = test_agent_config();
    existing.agent.hooks = Some(HooksConfig { log_all_tool_calls: false, block_tools: vec![], webhooks: vec![WebhookConfig { url: "https://keep/h".into(), ..Default::default() }] });
    // payload omitted webhooks → preserve
    preserve_hooks_webhooks(&mut new_cfg, &existing, false);
    assert_eq!(new_cfg.agent.hooks.as_ref().unwrap().webhooks.len(), 1);
    assert_eq!(new_cfg.agent.hooks.as_ref().unwrap().webhooks[0].url, "https://keep/h");
    // payload provided webhooks → keep new (empty)
    let mut new2 = new_cfg.clone();
    preserve_hooks_webhooks(&mut new2, &existing, true);
    assert_eq!(new2.agent.hooks.as_ref().unwrap().webhooks.len(), 1); // not re-cleared by this call alone — provided=true means leave as-is
}
```

(Точные сигнатуры test-helper-ов — по образцу существующих тестов crud.rs/config; если их нет, реализатор адаптирует тест под доступный харнесс и фиксирует в отчёте.)

- [ ] **Step 2: FAIL**

Run: `cargo test --bin opex-core preserve_webhooks -- --nocapture`
Expected: FAIL — `preserve_hooks_webhooks` не найдена.

- [ ] **Step 3: Реализация**

Извлечь чистую функцию (crud.rs):
```rust
/// Preserve existing webhooks when the PUT payload's hooks block omitted them
/// (mirror base/delegation preserve-from-disk). `payload_webhooks_present` =
/// payload.hooks.flatten().map(|h| h.webhooks.is_some()).
pub(crate) fn preserve_hooks_webhooks(new: &mut crate::config::AgentConfig, existing: &crate::config::AgentConfig, payload_webhooks_present: bool) {
    if payload_webhooks_present { return; }
    if let (Some(nh), Some(eh)) = (new.agent.hooks.as_mut(), existing.agent.hooks.as_ref()) {
        nh.webhooks = eh.webhooks.clone();
    } else if new.agent.hooks.is_none() {
        // hooks block omitted entirely → existing already preserved by hooks.is_none() branch
    }
}
```

В update-пути crud.rs (рядом с preserve base/delegation ~690-701, ПОСЛЕ build_agent_config, где доступен `existing_cfg`): вычислить `payload_webhooks_present = payload.hooks.as_ref().and_then(|h| h.as_ref()).map(|h| h.webhooks.is_some()).unwrap_or(false)` ДО потребления payload, затем `preserve_hooks_webhooks(&mut cfg, &existing_cfg, payload_webhooks_present);`. (Сверь точное имя переменной existing-конфига в crud.rs.)

- [ ] **Step 4: PASS**

Run: `cargo test --bin opex-core preserve_webhooks -- --nocapture` → PASS.
Run: `cargo check --all-targets` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/agents/crud.rs
git commit -m "fix(ui-hooks): preserve webhooks при PUT без webhooks (data-loss фикс)"
```

---

### Task 3: UI — webhooks-редактор в AgentEditDialog + always-send

**Files:**
- Modify: `ui/src/app/(authenticated)/agents/AgentEditDialog.tsx` (state ~111, рендер ~604-614)
- Modify: `ui/src/app/(authenticated)/agents/page.tsx` (`formToPayload` ~267-272, form-init)
- Test: `ui/src/app/(authenticated)/agents/__tests__/` (vitest) — новый или существующий

**Interfaces:**
- Consumes: `WebhookDto` из `api.generated.ts` (A1); preserve-backend (A2).

- [ ] **Step 1: Падающий vitest — редактор + сериализация**

Создать/дополнить тест (vitest + @testing-library/react). Пример (адаптируй под рендер AgentEditDialog/обёртки):

```tsx
import { render, screen, fireEvent } from "@testing-library/react";
// ... импорт AgentEditDialog + минимальные пропсы/обёртки

it("добавляет webhook-строку и показывает decision-поля при mode=decision", () => {
  render(<AgentEditDialogHarness initialHooks={{ log_all_tool_calls:false, block_tools:[], webhooks:[] }} />);
  fireEvent.click(screen.getByText(/Добавить webhook/i));
  fireEvent.change(screen.getByPlaceholderText(/https:\/\//i), { target: { value: "https://h/x" } });
  // mode=decision → появляются on_failure/timeout/allow_internal/tool_matcher
  fireEvent.click(screen.getByText(/decision/i));
  expect(screen.getByText(/on_failure|при сбое/i)).toBeInTheDocument();
});
```

(Если тестовая обёртка AgentEditDialog сложна — реализатор делает фокус-тест на выделенный sub-компонент `WebhooksEditor` (см. Step 3), что чище. Зафиксировать выбор в отчёте.)

- [ ] **Step 2: FAIL**

Run: `cd ui && npx vitest run src/app/\(authenticated\)/agents -t webhook`
Expected: FAIL.

- [ ] **Step 3: Реализация**

В `AgentEditDialog.tsx`: добавить в form-state `hooksWebhooks: WebhookDto[]` (init из агента). Вынести редактор в под-компонент `WebhooksEditor` (в том же файле или рядом) для тестируемости:
- список строк; на каждую: `Input` url; events — набор тоглов/чекбоксов (`BeforeMessage`/`BeforeToolCall`/`AfterToolResult`) → `string[]`; `Select` mode (async|decision); при `mode==="decision"`: `Input` tool_matcher, `Select` on_failure (open|closed), `Input type=number` timeout_ms, `Switch` allow_internal; кнопка «×» удалить.
- кнопка «+ Добавить webhook» → push дефолт `{url:"",events:[],mode:"async",tool_matcher:null,on_failure:"open",timeout_ms:3000,allow_internal:false}`.
Рендерить под существующей секцией hooks (после block_tools).

В `page.tsx` `formToPayload` — hooks ВСЕГДА включает webhooks:
```typescript
hooks: (f.hooksLogAll || f.hooksBlockTools.trim() || f.hooksWebhooks.length)
  ? {
      log_all_tool_calls: f.hooksLogAll,
      block_tools: splitList(f.hooksBlockTools),
      webhooks: f.hooksWebhooks,   // всегда полный массив (с backend preserve — нет data-loss)
    }
  : null,
```
И form-init из `agent.hooks.webhooks` (читается из GET-DTO после A1).

- [ ] **Step 4: PASS + сборка**

Run: `cd ui && npx vitest run src/app/\(authenticated\)/agents -t webhook` → PASS.
Run: `cd ui && npx tsc --noEmit` → без ошибок (типы WebhookDto совпадают).

- [ ] **Step 5: Commit**

```bash
git add ui/src/app/\(authenticated\)/agents/AgentEditDialog.tsx ui/src/app/\(authenticated\)/agents/page.tsx ui/src/app/\(authenticated\)/agents/__tests__/
git commit -m "feat(ui-hooks): редактор decision-webhooks в AgentEditDialog (always-send webhooks)"
```

---

## Self-Review

**1. Spec coverage:** §Hooks DTO (WebhookDto + webhooks) → A1; CRIT preserve-on-omit → A2; UI-редактор always-send → A3; ts-rs codegen → A1; mode/on_failure lowercase → A1 (map) + A3 (form). ✓
**2. Placeholder scan:** код в шагах полный; test-helper-имена A2/A3 помечены «адаптировать под харнесс» (реальные имена зависят от существующих тестов — discovery, не плейсхолдер).
**3. Type consistency:** `WebhookDto` поля идентичны A1 (Rust) ↔ A3 (TS, из codegen). `hooksWebhooks: WebhookDto[]` A3. preserve `payload_webhooks_present` A2. ✓
