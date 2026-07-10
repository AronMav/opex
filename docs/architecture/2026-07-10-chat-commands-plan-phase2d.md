# Chat Command Registry — Фаза 2d (argsMenu inline-кнопки + click-to-run) — план

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Команда-обработчик с choice-валвом (напр. `/summarize_video <url>`, валв `summary_length` = short|medium|long) при вызове показывает интерактивное меню-кнопки выбора значения; клик по кнопке (Telegram inline / web card) завершает команду и ставит handler_job с выбранным значением. Фича делается ЖИВОЙ (реальный choice-валв на `summarize_video`), не спящей.

**Architecture:** (1) toolgate `descriptor.py` учится парсить `choices` на `<config><field>`; `summarize_video` получает реальный choice-валв `summary_length` и использует его в `run()`. (2) Ядро: `try_handler_command`, разрешив источник и пройдя трастовый гейт, если у команды есть незаполненный choice-валв (`menu:true` арг с `choices`) — вместо enqueue эмитит `command_args_menu` с `options` + `store_menu_ctx`-токеном (стэш: handler_id, source, session, agent, valve_name). (3) Новый эндпоинт `/api/commands/menu-run {token, value}` восстанавливает стэш → `params[valve_name]=value` → enqueue через переиспользуемый `menu_run_core` (расширен `extra_params`). (4) Web-карточка и Telegram рендерят `options` как кнопки; клик → run-эндпоинт (мод. `hm:`/`cm:`-паттерна).

**Tech Stack:** Python (toolgate), Rust 2024 (opex-core), TypeScript (channels), Next.js/React (ui). rustls only.

**Основа (задеплоено):** Ф.2a `derive_handler_commands` уже читает `f.get("choices")` из config-поля → делает choice-арг `menu:true`; но `descriptor.py` пока НЕ парсит `choices` (пробел, закрываем в T1). `command_args_menu` rich-card рендерится на web (`ui/src/components/ui/command-args-menu-card.tsx`, кнопки display-only) и на канале доставляется текстом (Ф.2a-фикс). `store_menu_ctx(params: Value) -> String` (32-hex токен, 30-мин TTL) + `menu_run_core(infra, handlers, source_url, upload_id, session, agent, handler_id)` + `MENU_CTX` (`gateway/handlers/files.rs`). Telegram `hm:<token>:<handler_id>` callback → POST `/api/files/menu-run {token, handler_id, chat_id}` (`channels/src/drivers/telegram.ts:462`); `send_buttons` action → InlineKeyboard.

## Global Constraints

- Rust 2024; rustls only, no OpenSSL; no new `.env` keys.
- **Трастовый гейт (F6) сохраняется:** enqueue только после `match_url_handlers`/`match_buttons` по РЕЗОЛВНУТОМУ handler_id (как Ф.2a/2c). Токен-стэш несёт handler_id; run-эндпоинт пере-валидирует через `menu_run_core` (ownership + matched-set). Токен 30-мин TTL, unguessable.
- **MVP:** поддерживаем ОДИН choice-валв на команду (первый `menu:true`-арг с choices). Больше — отдельно.
- Choice-значение из кнопки валидируется: должно быть среди объявленных `choices` валва (иначе reject).
- Гейты: Rust `cargo clippy --all-targets -p opex-core -- -D warnings` CLEAN; toolgate `python -m pytest` (venv локально/сервер); channels `bun test`; ui `npm test`+`npm run build` (из `ui/`).
- Серверный Rust-тест bounded (`--bin opex-core`, `CARGO_BUILD_JOBS=4 nice ionice`, детачед). Коммиты: 1/задача, без `Co-Authored-By`, master; push/деплой — только с явного разрешения.

---

## Файловая структура (2d)

**Модифицируется:**
- `toolgate/handlers/descriptor.py` — config-field parse +`choices`.
- `toolgate/handlers/builtin/summarize_video.py` — descriptor +`<field name="summary_length" ... choices="short,medium,long"/>`; `run()` использует `ctx.config.get("summary_length")`.
- `crates/opex-core/src/agent/commands/dispatch.rs` — эмиссия `command_args_menu` для незаполненного choice-валва + токен-стэш.
- `crates/opex-core/src/gateway/handlers/files.rs` — `menu_run_core` +`extra_params: serde_json::Value`; новый `/api/commands/menu-run` handler.
- `crates/opex-core/src/gateway/handlers/commands.rs` (или `mod.rs`) — route `/api/commands/menu-run`.
- `ui/src/components/ui/command-args-menu-card.tsx` — кнопки → POST run-эндпоинт.
- `channels/src/drivers/telegram.ts` — `command_args_menu` → inline-кнопки (`cm:<token>:<value>`) + callback → run-эндпоинт.

**Создаётся:**
- `toolgate/tests/` — тест choices-парсинга (в существующем descriptor-тесте).
- `channels/src/drivers/telegram-argsmenu.ts` + тест — чистый маппинг options→InlineKeyboard buttons.

---

## Task 1: toolgate — `choices` на валве + реальный choice-валв на summarize_video

**Files:**
- Modify: `toolgate/handlers/descriptor.py` (config-field +choices)
- Modify: `toolgate/handlers/builtin/summarize_video.py` (валв + run)
- Test: `toolgate/tests/test_handlers_descriptor.py`

**Interfaces:**
- Produces: config-field dict получает `"choices": [str] | None` (comma-split из `choices` атрибута). `/handlers` манифест `config[i].choices` доступен.

- [ ] **Step 1: Падающий тест choices-парсинга**

В `toolgate/tests/test_handlers_descriptor.py`:

```python
def test_config_field_choices_parsed():
    src = """
# <handler>
#   <id>x</id>
#   <label lang="en">X</label>
#   <match><mime>audio/*</mime></match>
#   <execution>async</execution>
#   <config>
#     <field name="quality" type="string" default="high" label="Q" choices="low,high"/>
#   </config>
# </handler>
"""
    d = parse_descriptor(src, tier="workspace")
    field = d.config[0]
    assert field["choices"] == ["low", "high"]

def test_config_field_no_choices_is_none():
    src = """
# <handler>
#   <id>x</id>
#   <label lang="en">X</label>
#   <match><mime>audio/*</mime></match>
#   <execution>async</execution>
#   <config>
#     <field name="lang" type="string" default="ru" label="L"/>
#   </config>
# </handler>
"""
    assert parse_descriptor(src, tier="workspace").config[0]["choices"] is None
```

- [ ] **Step 2: Прогнать — падает**

Run (venv): `python -m pytest toolgate/tests/test_handlers_descriptor.py -k choices -q` → FAIL (нет ключа `choices`).

- [ ] **Step 3: Реализовать**

В `descriptor.py` config-field append (около строки 138), добавить в dict:
```python
"choices": (
    [c.strip() for c in f.get("choices", "").split(",") if c.strip()]
    or None
),
```

В `summarize_video.py` descriptor: добавить в `<config>` поле
```
#     <field name="summary_length" type="string" default="medium" label="Длина конспекта" description="short | medium | long" choices="short,medium,long"/>
```
В `run(ctx, file, params)` `summarize_video.py`: прочитать `length = (ctx.config or {}).get("summary_length", "medium")` и подмешать в digest-промпт (напр. short → «краткий, 5-7 пунктов», medium → текущий, long → «подробный»). Минимально: добавить строку в промпт digest в зависимости от `length`.

- [ ] **Step 4: Прогнать — зелёные**

Run: `python -m pytest toolgate/tests/test_handlers_descriptor.py -k choices -q` → PASS.

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/descriptor.py toolgate/handlers/builtin/summarize_video.py toolgate/tests/test_handlers_descriptor.py
git commit -m "feat(toolgate): parse <field choices> valve + summary_length choice-valve on summarize_video"
```

---

## Task 2: Ядро — эмиссия argsMenu для choice-валва + токен-стэш

**Files:**
- Modify: `crates/opex-core/src/agent/commands/dispatch.rs`
- Test: инлайн (чистый выбор «есть незаполненный choice-арг?»)

**Interfaces:**
- Consumes: `derive_handler_commands` (choice-арг из валва), `store_menu_ctx`, `HandlerManifest.config`.
- Produces: когда у резолвнутой команды есть `menu:true`-арг с `choices` и значение не задано → `try_handler_command` возвращает `CommandOutcome::Menu { card }` где `card = {"card_type":"command_args_menu","command":<name>,"text":<prompt>,"options":[{value,label}],"token":<t>}`; иначе (нет choice-валва) — прежний enqueue.

- [ ] **Step 1: Падающий тест хелпера выбора choice-арга**

Извлечь чистый хелпер и протестировать:
```rust
#[test]
fn first_choice_valve_detected() {
    // manifest with a config field carrying choices → returns (name, [choices])
    let m: crate::agent::handler_registry::HandlerManifest = serde_json::from_value(serde_json::json!({
        "id":"summarize_video","execution":"async","tier":"workspace","descriptions":{"en":"d"},
        "config":[{"name":"summary_length","type":"string","choices":["short","medium","long"]}]
    })).unwrap();
    let got = super::first_choice_valve(&m);
    assert_eq!(got, Some(("summary_length".to_string(), vec!["short".into(),"medium".into(),"long".into()])));
}
#[test]
fn no_choice_valve_is_none() {
    let m: crate::agent::handler_registry::HandlerManifest = serde_json::from_value(serde_json::json!({
        "id":"x","execution":"async","tier":"workspace","descriptions":{"en":"d"},
        "config":[{"name":"lang","type":"string"}]
    })).unwrap();
    assert_eq!(super::first_choice_valve(&m), None);
}
```

- [ ] **Step 2: Прогнать — падает**

Run: `cargo test -p opex-core commands::dispatch::tests::first_choice_valve -- --nocapture` (server) / локально `cargo check`.

- [ ] **Step 3: Реализовать**

В `dispatch.rs`:
```rust
/// First operator valve that declares `choices` (MVP: single choice-valve per command).
pub(crate) fn first_choice_valve(m: &crate::agent::handler_registry::HandlerManifest) -> Option<(String, Vec<String>)> {
    m.config.as_array()?.iter().find_map(|f| {
        let name = f.get("name")?.as_str()?.to_string();
        let choices: Vec<String> = f.get("choices")?.as_array()?
            .iter().filter_map(|v| v.as_str().map(String::from)).collect();
        (!choices.is_empty()).then_some((name, choices))
    })
}
```
В `try_handler_command`, ПОСЛЕ резолва источника + успешного трастового гейта (`gated_ok == true`), но ПЕРЕД `insert_handler_job`: если `first_choice_valve(target_manifest)` = `Some((valve, choices))` → построить стэш и вернуть Menu:
```rust
if let Some((valve, choices)) = first_choice_valve(m) {
    let params = serde_json::json!({
        "kind":"command_choice", "handler_id": handler_id,
        "source_url": source_ref, "upload_id": upload.map(|u| u.to_string()),
        "session_id": deps.session_id_resolved.to_string(), "agent": deps.agent_name,
        "valve": valve, "choices": choices, "language": lang,
    });
    let token = crate::gateway::handlers::files::store_menu_ctx(params);
    let card = serde_json::json!({
        "card_type":"command_args_menu", "command": name,
        "text": format!("Выберите значение «{valve}» для /{name}:"),
        "options": choices.iter().map(|c| serde_json::json!({"value":c,"label":c})).collect::<Vec<_>>(),
        "token": token,
    });
    return Some(Ok(CommandOutcome::Menu { card }));
}
// else: existing enqueue path
```
(Если source не резолвнут — прежний missing-source Menu без options/token остаётся, как в 2a.)

- [ ] **Step 4: clippy gate**

Run: `cargo clippy --all-targets -p opex-core -- -D warnings` CLEAN.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/dispatch.rs
git commit -m "feat(commands): emit command_args_menu with choice-valve options + token"
```

---

## Task 3: Ядро — `/api/commands/menu-run` (click-to-run enqueue)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/files.rs` (`menu_run_core` +extra_params; новый handler)
- Modify: route registration (`gateway/handlers/mod.rs` или `commands.rs::routes`)
- Test: инлайн — расширенный `menu_run_core` мержит extra_params в job params

**Interfaces:**
- Produces: `POST /api/commands/menu-run {token, value}` → восстанавливает стэш по токену, валидирует `value ∈ choices`, `params = {language, <valve>: value}`, вызывает `menu_run_core(..., extra_params=params)` (ownership + matched-set гейт + enqueue). Ответ `{ok:true}` / ошибка.

- [ ] **Step 1: Реализовать `menu_run_core` extra_params + handler**

`menu_run_core`: добавить параметр `extra_params: serde_json::Value` и при формировании job-params влить его (текущее `params = {"language": lang}` → расширить полями из `extra_params`). Все текущие вызовы `menu_run_core` передают `json!({})`.

Новый handler:
```rust
#[derive(Deserialize)]
struct CommandMenuRunRequest { token: String, value: String, #[serde(default)] chat_id: Option<serde_json::Value> }

async fn command_menu_run(
    State(infra): State<InfraServices>, State(handlers): State<HandlerRegistry>,
    Json(req): Json<CommandMenuRunRequest>,
) -> impl IntoResponse {
    let ctx = menu_ctx().lock().ok().and_then(|m| m.get(&req.token).map(|(v,_)| v.clone()));
    let Some(ctx) = ctx else { return (StatusCode::BAD_REQUEST, Json(json!({"error":"expired token"}))).into_response(); };
    // validate value ∈ choices
    let choices: Vec<String> = ctx.get("choices").and_then(|c| c.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    if !choices.iter().any(|c| c == &req.value) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid choice"}))).into_response();
    }
    let handler_id = ctx.get("handler_id").and_then(|v| v.as_str()).unwrap_or_default();
    let session_id = ctx.get("session_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let agent = ctx.get("agent").and_then(|v| v.as_str()).unwrap_or_default();
    let source_url = ctx.get("source_url").and_then(|v| v.as_str());
    let upload_id = ctx.get("upload_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let valve = ctx.get("valve").and_then(|v| v.as_str()).unwrap_or_default();
    let Some(session_id) = session_id else { return (StatusCode::BAD_REQUEST, Json(json!({"error":"bad ctx"}))).into_response(); };
    let extra = json!({ valve: req.value });
    menu_run_core(&infra, &handlers, source_url, upload_id, session_id, agent, handler_id, extra).await
}
```
Route: `.route("/api/commands/menu-run", post(command_menu_run))` (под `/api/*` auth).

- [ ] **Step 2: clippy + inline test**

Тест: `menu_run_core` с `extra_params = json!({"summary_length":"long"})` → job params содержат `summary_length: "long"` (можно юнит на функцию слияния params, без DB — вынести merge в чистый хелпер). `cargo clippy --all-targets -p opex-core -- -D warnings` CLEAN.

- [ ] **Step 3: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/files.rs crates/opex-core/src/gateway/handlers/mod.rs
git commit -m "feat(api): /api/commands/menu-run completes a choice-valve command (validate + enqueue)"
```

---

## Task 4: Web-карточка — клик по кнопке → run

**Files:**
- Modify: `ui/src/components/ui/command-args-menu-card.tsx`
- Test: `ui/src/components/ui/command-args-menu-card.test.tsx`

**Interfaces:**
- Consumes: `data.token`, `data.options`. POST `/api/commands/menu-run {token, value}` через `apiPost`.

- [ ] **Step 1: Падающий тест (клик постит value)**

Дополнить тест: мокнуть `apiPost`, клик по кнопке `long` → `apiPost` вызван с `("/api/commands/menu-run", {token:"t", value:"long"})`.

- [ ] **Step 2: Прогнать — падает**

Run (из `ui/`): `npm test -- command-args-menu-card` → FAIL (кнопки display-only).

- [ ] **Step 3: Реализовать**

Кнопки: `onClick={() => data.token && apiPost("/api/commands/menu-run", { token: data.token, value: o.value }).catch(()=>{})}`; после клика — задизейблить/отметить выбранное (простой local state). Импорт `apiPost` из `@/lib/api`.

- [ ] **Step 4: Прогнать + build**

Run (из `ui/`): `npm test -- command-args-menu-card` PASS; `npm run build` clean.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/ui/command-args-menu-card.tsx ui/src/components/ui/command-args-menu-card.test.tsx
git commit -m "feat(ui): command_args_menu buttons run the command via /api/commands/menu-run"
```

---

## Task 5: Telegram — inline-кнопки argsMenu + callback

**Files:**
- Create: `channels/src/drivers/telegram-argsmenu.ts` + `.test.ts`
- Modify: `channels/src/drivers/telegram.ts` (Menu → send_buttons; `cm:` callback)
- Modify: `crates/opex-core/src/agent/pipeline/sink.rs` (ChannelStatusSink уже ловит `command_args_menu` — конвертация в send_buttons на канальном пути)

**Interfaces:**
- Produces: `argsMenuToButtons(card) -> {text, buttons:[{text,data}]}` где `data = cm:<token>:<value>`; Telegram callback `cm:<token>:<value>` → POST `/api/commands/menu-run {token, value, chat_id}`.

- [ ] **Step 1: Падающий bun-тест маппинга**

`telegram-argsmenu.test.ts`: `argsMenuToButtons({token:"t", options:[{value:"short",label:"short"}], text:"Выберите"})` → `{text:"Выберите", buttons:[{text:"short", data:"cm:t:short"}]}`.

- [ ] **Step 2: Прогнать — падает** (из `channels/`): `bun test telegram-argsmenu` → FAIL.

- [ ] **Step 3: Реализовать**

`telegram-argsmenu.ts`: `argsMenuToButtons`. В `telegram.ts`: где канальный путь получает `command_args_menu` (через ChannelStatusSink.menu → уже приходит как action?), сконвертировать в `send_buttons` action с `argsMenuToButtons`. В `callback_query:data` добавить ветку `data.startsWith("cm:")`: split → `{token, value}` → POST `${coreUrl}/api/commands/menu-run {token, value, chat_id}` (паттерн `hm:` на telegram.ts:462), `answerCallbackQuery` + `editMessageReplyMarkup`.

  **Core-сторона:** `command_args_menu` с `options` должен доехать до канала как `send_buttons`. Проверить путь: `ChannelStatusSink.menu` (ловит card, sink.rs:144) → как `handler_menu` конвертируется в send_buttons на канальном finalize? Мод. этот же путь для `command_args_menu` с options → `send_buttons` (кнопки из `options`, data `cm:<token>:<value>`). Если такого пути нет (2a-фикс слал только текст), добавить: при наличии `menu.options` эмитить `send_buttons` ChannelAction.

- [ ] **Step 4: Прогнать + typecheck**

Run (из `channels/`): `bun test` (зелёный) + `bunx tsc --noEmit` clean. Rust `cargo clippy --all-targets -p opex-core -- -D warnings` CLEAN.

- [ ] **Step 5: Commit**

```bash
git add channels/src/drivers/telegram-argsmenu.ts channels/src/drivers/telegram-argsmenu.test.ts channels/src/drivers/telegram.ts crates/opex-core/src/agent/pipeline/sink.rs
git commit -m "feat(channels): render command_args_menu as Telegram inline buttons + cm: callback run"
```

---

## Task 6: Серверный тест, деплой, E2E

- [ ] **Step 1:** Bounded server Rust test (`--bin opex-core commands`) — все зелёные incl. `first_choice_valve`. toolgate `-k choices` PASS. UI `npm test`+build. channels `bun test`.
- [ ] **Step 2:** Деплой (с разрешения): push → `server-deploy.sh` (Rust + toolgate + channels sync + рестарт).
- [ ] **Step 3:** E2E: `/summarize_video <youtube-url>` в web → карточка с кнопками [short][medium][long] → клик → job ставится с `summary_length`. В Telegram — inline-кнопки → клик → job. Проверить, что digest учитывает длину.
- [ ] **Step 4:** Маркер: `git commit --allow-empty -m "chore(commands): Phase 2d complete — argsMenu choice-valve buttons + click-to-run"`.

---

## Не в объёме 2d (→ позже)

- Мульти-choice-валв меню (последовательные выборы) — MVP один валв.
- Discord — Фаза 3.

## Self-Review (2d)

- **Покрытие:** choices-парсинг + реальный валв (T1 ✓), эмиссия argsMenu+token (T2 ✓), click-to-run эндпоинт (T3 ✓), web-клик (T4 ✓), Telegram-кнопки+callback (T5 ✓), тест/деплой/E2E (T6 ✓).
- **Плейсхолдеры:** нет. Два места «проверить существующий путь» (канальный send_buttons для command_args_menu в T5; params-merge в menu_run_core в T3) — конкретные верификации с fallback-инструкцией, не догадки.
- **Согласованность:** `store_menu_ctx`/`menu_run_core(+extra_params)`/`first_choice_valve`/`/api/commands/menu-run {token,value}`/`cm:<token>:<value>`/`argsMenuToButtons` — согласованы. Трастовый гейт по handler_id из стэша сохраняется (T3 через menu_run_core).
- **Риск:** средний — кросс-подсистемно; трастовый гейт переиспользует проверенный `menu_run_core`; choice-value валидируется против стэша.
