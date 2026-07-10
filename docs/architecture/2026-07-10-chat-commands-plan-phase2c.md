# Chat Command Registry — Фаза 2c (M1 shared-registry + `<command>`-оверрайд) — план

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** (M1) диспетч handler-команд и `/help` переиспользуют общий `HandlerRegistry` (ETag-reuse) вместо свежего toolgate-fetch на каждое `/`-сообщение; (`<command>`) toolgate-обработчик может задать кастомное имя/алиасы команды через `<command>` в дескрипторе.

**Architecture:** M1 — прокидываем `AppState.handlers` (общий `HandlerRegistry`, внутри `Arc<RwLock>` — clone разделяет ETag-кэш) в `agent_config::AgentConfig`; `try_handler_command` и `/help` берут его из `cfg()` вместо `HandlerRegistry::new(...)`. `<command>` — `descriptor.py` парсит `<command name="..." aliases="a,b"/>` в манифест; Rust `HandlerManifest` получает поле `command`; `derive_handler_commands` применяет оверрайд (имя+алиасы), сохраняя `source: Handler{handler_id=id}` (id обработчика неизменен — enqueue по нему).

**Tech Stack:** Rust 2024 (opex-core), Python (toolgate). rustls only.

**Основа:** Фазы 1/2a/2b задеплоены. `HandlerRegistry` (`agent/handler_registry.rs`): `.refresh()`, `.manifests()`, `.etag()`, Clone (shared `Arc<RwLock<HandlerCache>>`). `try_handler_command` (`agent/commands/dispatch.rs`) и `/help` (`agent/pipeline/commands.rs`) сейчас строят `HandlerRegistry::new(toolgate_url, http)`. `derive_handler_commands` (`agent/commands/handler_source.rs`). Дескриптор-парсер `toolgate/handlers/descriptor.py` (`parse_descriptor`, ElementTree; `HandlerDescriptor` dataclass с `config: list[dict]`).

## Global Constraints

- Rust 2024; rustls only, no OpenSSL; no new `.env` keys.
- **M1 не должен менять поведение** диспетча/`/help` (только источник манифестов — общий реестр вместо свежего). Fail-soft сохраняется.
- **`<command>` не меняет `handler_id`** — enqueue всегда по id обработчика; оверрайд влияет только на имя/алиасы КОМАНДЫ. Имена/алиасы валидируются `[a-zA-Z0-9_-]`; приоритет builtin (`build_registry`) остаётся — конфликтный оверрайд-алиас дропается.
- Rust-гейт локально: `cargo clippy --all-targets -p opex-core -- -D warnings` CLEAN (clippy на Windows работает; `cargo test` — нет → bounded server pass). toolgate-тесты: `~/opex/toolgate/.venv` на сервере ИЛИ локально если venv есть; часть тест-депов может отсутствовать.
- Серверный Rust-тест: bounded (`--bin opex-core`, `CARGO_BUILD_JOBS=4 nice ionice`, детачед).
- Коммиты: 1/задача, без `Co-Authored-By`, master; push/деплой — только с явного разрешения.

---

## Файловая структура (2c)

**Модифицируется:**
- `crates/opex-core/src/agent/agent_config.rs` — `AgentConfig` +поле `handler_registry: crate::agent::handler_registry::HandlerRegistry`.
- Место конструкции `agent_config::AgentConfig` (где доступен `AppState.handlers` — engine startup/spawn; grep `agent_config::AgentConfig {` / основной конструктор в `main.rs`/engine setup) — прокинуть clone `AppState.handlers`.
- `crates/opex-core/src/agent/commands/dispatch.rs` — `HandlerDispatchDeps` берёт `&HandlerRegistry` (или переиспользует общий) вместо `toolgate_url`+`http`; `try_handler_command` использует его.
- `crates/opex-core/src/agent/engine/context_builder.rs` — обёртка `handle_command` строит deps с `&self.cfg().handler_registry`.
- `crates/opex-core/src/agent/pipeline/commands.rs` — `CommandContext` берёт `&HandlerRegistry` для `/help` (вместо `toolgate_url`/`http`).
- `toolgate/handlers/descriptor.py` — `HandlerDescriptor` +`command: dict | None`; `parse_descriptor` парсит `<command>`.
- `crates/opex-core/src/agent/handler_registry.rs` — `HandlerManifest` +`command: Option<CommandOverride>` (`#[serde(default)]`).
- `crates/opex-core/src/agent/commands/handler_source.rs` — `derive_handler_commands` применяет `command`-оверрайд.

---

## Task 1: M1 — общий `HandlerRegistry` в движке (dispatch + /help)

**Files:**
- Modify: `crates/opex-core/src/agent/agent_config.rs` (поле)
- Modify: конструктор `agent_config::AgentConfig` (инъекция из `AppState.handlers`)
- Modify: `crates/opex-core/src/agent/commands/dispatch.rs`, `engine/context_builder.rs`, `pipeline/commands.rs`
- Test: инлайн unit где возможно; полный путь — bounded server

**Interfaces:**
- Produces: `AgentConfig.handler_registry: HandlerRegistry`. `HandlerDispatchDeps` несёт `handlers: &'a HandlerRegistry` (замена `toolgate_url`/`http`). `/help` использует `ctx.handlers: &HandlerRegistry`.

- [ ] **Step 1: Добавить поле `handler_registry` в `AgentConfig`**

В `agent_config.rs` добавить `pub handler_registry: crate::agent::handler_registry::HandlerRegistry,` в `struct AgentConfig`. Найти основной конструктор `AgentConfig { ... }` (где строится движок и доступен `AppState`/`handlers`) и передать `handler_registry: <AppState>.handlers.clone()`. Все прочие места конструкции `AgentConfig` (тестовые/CRUD, что строят усечённый `AgentConfig { agent: ... }`) — если это ДРУГОЙ тип (`config::mod::AgentConfig`), НЕ трогать; если тот же — добавить поле.

- [ ] **Step 2: `HandlerDispatchDeps` берёт `&HandlerRegistry`**

В `dispatch.rs`: заменить в `HandlerDispatchDeps` поля `toolgate_url: String` + `http: reqwest::Client` на `handlers: &'a HandlerRegistry`. В `try_handler_command` убрать `let reg = HandlerRegistry::new(...); reg.refresh().await;` → использовать `deps.handlers.refresh().await; let manifests = deps.handlers.manifests().await;` (refresh на общем реестре = conditional GET с сохранённым ETag).

- [ ] **Step 3: Обёртка + `/help` прокидывают общий реестр**

В `context_builder.rs::handle_command`: строить `HandlerDispatchDeps { handlers: &self.cfg().handler_registry, db: &self.cfg().db, agent_name, agent_language, ... }`. В `pipeline/commands.rs`: `CommandContext` заменить `toolgate_url: Option<String>`/`http: Option<reqwest::Client>` на `handlers: Option<&'a HandlerRegistry>` (или всегда `&HandlerRegistry`); `/help` использует его для `derive_handler_commands`. Обёртка передаёт `Some(&self.cfg().handler_registry)`.

- [ ] **Step 4: clippy gate**

Run: `cargo clippy --all-targets -p opex-core -- -D warnings` — CLEAN.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/agent_config.rs crates/opex-core/src/agent/commands/dispatch.rs crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/agent/pipeline/commands.rs
git commit -m "perf(commands): reuse shared HandlerRegistry in dispatch + /help (ETag reuse, no per-message fetch)"
```

---

## Task 2: `<command>` — toolgate дескриптор-парсинг

**Files:**
- Modify: `toolgate/handlers/descriptor.py`
- Test: `toolgate/tests/test_descriptor.py` (или существующий descriptor-тест — найти по `parse_descriptor`)

**Interfaces:**
- Produces: `HandlerDescriptor.command: dict | None` = `{"name": str, "aliases": [str], "description": {lang:str}}` при наличии `<command>`; включается в JSON манифеста (`/handlers`).

- [ ] **Step 1: Падающий тест**

Найти существующий тест `parse_descriptor` (grep `parse_descriptor` в toolgate/tests). Добавить:

```python
def test_command_override_parsed():
    src = """
# <handler>
#   <id>summarize_video</id>
#   <execution>async</execution>
#   <command name="sumvid" aliases="sv,summary"/>
# </handler>
"""
    d = parse_descriptor(src, tier="workspace")
    assert d.command == {"name": "sumvid", "aliases": ["sv", "summary"]}

def test_no_command_is_none():
    src = "# <handler>\n#   <id>x</id>\n#   <execution>async</execution>\n# </handler>"
    assert parse_descriptor(src, tier="workspace").command is None
```

- [ ] **Step 2: Прогнать — падает**

Run (venv): `python -m pytest toolgate/tests/test_descriptor.py -k command -q`
Expected: FAIL — `HandlerDescriptor` не имеет `command` / не парсится.

- [ ] **Step 3: Реализовать в descriptor.py**

1. В `HandlerDescriptor` dataclass добавить `command: dict | None = None`.
2. В `parse_descriptor`, после блока `config`, добавить:
   ```python
   command = None
   cmd_el = root.find("command")
   if cmd_el is not None:
       name = (cmd_el.get("name") or "").strip()
       aliases = [a.strip() for a in (cmd_el.get("aliases") or "").split(",") if a.strip()]
       if name:
           command = {"name": name, "aliases": aliases}
   ```
3. Включить `command` в возвращаемый `HandlerDescriptor(...)` и в JSON-сериализацию манифеста (там, где dataclass→dict для `/handlers`; если через `dataclasses.asdict`/явный dict — добавить `"command": self.command`).

- [ ] **Step 4: Прогнать — зелёные**

Run: `python -m pytest toolgate/tests/test_descriptor.py -k command -q` → PASS.

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/descriptor.py toolgate/tests/test_descriptor.py
git commit -m "feat(toolgate): parse optional <command> descriptor override (custom name/aliases)"
```

---

## Task 3: `<command>`-оверрайд в деривации (Rust)

**Files:**
- Modify: `crates/opex-core/src/agent/handler_registry.rs` (`HandlerManifest` +`command`)
- Modify: `crates/opex-core/src/agent/commands/handler_source.rs` (`derive_handler_commands` применяет)
- Test: инлайн в `handler_source.rs`

**Interfaces:**
- Consumes: `HandlerManifest.command`.
- Produces: `derive_handler_commands` использует `command.name` как имя команды (если задан) + `command.aliases` как алиасы; `source` остаётся `Handler{handler_id: m.id}` (НЕ имя команды).

- [ ] **Step 1: Падающий тест**

В `handler_source.rs` тесты:

```rust
#[test]
fn command_override_sets_name_and_aliases_but_keeps_handler_id() {
    use serde_json::json;
    let m: crate::agent::handler_registry::HandlerManifest = serde_json::from_value(json!({
        "id":"summarize_video","execution":"async","tier":"workspace",
        "descriptions":{"en":"d"},"config":[],
        "command":{"name":"sumvid","aliases":["sv"]}
    })).unwrap();
    let specs = derive_handler_commands(&[m], &[], "en");
    assert_eq!(specs[0].name, "sumvid");
    assert_eq!(specs[0].aliases, vec!["sv".to_string()]);
    match &specs[0].source {
        CommandSourceKind::Handler { handler_id } => assert_eq!(handler_id, "summarize_video"),
        _ => panic!("expected Handler source"),
    }
}
```

- [ ] **Step 2: Прогнать — падает** (`command` не десериализуется / не применяется)

Run: `cargo test -p opex-core commands::handler_source -- --nocapture` (server bounded) / локально `cargo check --all-targets -p opex-core`.

- [ ] **Step 3: Реализовать**

1. `handler_registry.rs`: в `HandlerManifest` добавить
   ```rust
   #[serde(default)]
   pub command: Option<CommandOverride>,
   ```
   + `#[derive(Debug, Clone, Deserialize)] pub struct CommandOverride { pub name: String, #[serde(default)] pub aliases: Vec<String> }`.
2. `handler_source.rs` `derive_handler_commands`: для каждого манифеста — если `m.command` есть, использовать `command.name` как `name` и `command.aliases` (валидные `[a-zA-Z0-9_-]`) как `aliases`; иначе `name = m.id`, `aliases = vec![]`. `source: CommandSourceKind::Handler { handler_id: m.id.clone() }` — БЕЗ изменений.

- [ ] **Step 4: clippy gate**

Run: `cargo clippy --all-targets -p opex-core -- -D warnings` — CLEAN.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/handler_registry.rs crates/opex-core/src/agent/commands/handler_source.rs
git commit -m "feat(commands): apply <command> override (custom name/aliases) in handler-command derivation"
```

---

## Task 4: Серверный тест, деплой, E2E

**Files:** нет новых.

- [ ] **Step 1: Bounded server Rust test**

bundle→worktree, `CARGO_BUILD_JOBS=4 nice -n 19 ionice -c3` детачед, `cargo test -p opex-core --bin opex-core commands -- --nocapture`. Ожидание: все команд-тесты (в т.ч. новый `command_override_...`) зелёные. Локально предварительно `cargo clippy --all-targets -p opex-core -- -D warnings` CLEAN.

- [ ] **Step 2: toolgate test (server)**

На сервере: `~/opex/toolgate/.venv/bin/python -m pytest ~/opex/toolgate/tests/test_descriptor.py -k command -q` (или локально при наличии venv). Ожидание: PASS.

- [ ] **Step 3: Деплой (с разрешения пользователя)**

Push origin → `bash ~/opex-src/scripts/server-deploy.sh` (Rust + синк toolgate `.py` + рестарт; core пере-подхватит манифесты с новым `command`-полем через HandlerRegistry conditional GET).

- [ ] **Step 4: E2E**

- M1: `/help` и `/summarize_video <url>` работают как раньше (паритет); при повторных `/`-сообщениях toolgate НЕ получает полный manifest-fetch каждый раз (ETag 304 — проверить по toolgate-логу `GET /handlers` статусам, если логируются).
- `<command>`: добавить в тестовый workspace-обработчик `<command name="sv"/>`, дождаться hot-reload, `curl /api/commands` → команда `sv` присутствует, `summarize_video` (id) — под оверрайдом. `/sv <url>` ставит job (enqueue по handler_id).

- [ ] **Step 5: Маркер завершения 2c**

```bash
git commit --allow-empty -m "chore(commands): Phase 2c complete — shared HandlerRegistry reuse + <command> override"
```

---

## Не в объёме 2c (→ Фаза 2d)

- **argsMenu inline-кнопки + click-to-run** (Telegram + web): бэкенд-эмиссия option-меню для choice-аргов (валвсов) + обобщённый `menu_run_core`-путь под завершение команды + `store_menu_ctx`-токен + Telegram `cm:<token>:<value>` callback + web-карточка click. Отдельный план (крупная кросс-подсистемная фича).
- Discord — Фаза 3.

## Self-Review (2c)

- **Покрытие:** M1 shared-registry (T1 ✓), `<command>` toolgate-парсинг (T2 ✓), `<command>` деривация Rust (T3 ✓), тест/деплой/E2E (T4 ✓).
- **Плейсхолдеры:** нет. Один шаг (T1 Step 1) — «найти основной конструктор `AgentConfig`» — конкретный lookup (grep указан), не догадка; типовая коллизия `agent_config::AgentConfig` vs `config::AgentConfig` явно отмечена.
- **Согласованность типов:** `HandlerDispatchDeps.handlers: &HandlerRegistry`, `CommandContext.handlers`, `AgentConfig.handler_registry`, `HandlerManifest.command: Option<CommandOverride>`, `derive_handler_commands` — согласованы. `handler_id` неизменен (enqueue-безопасность).
- **Риск:** M1 — паритетный рефактор (источник манифестов), clippy ловит. `<command>` — аддитивно (`#[serde(default)]`, опционально). Оба покрыты тестами + bounded server.
