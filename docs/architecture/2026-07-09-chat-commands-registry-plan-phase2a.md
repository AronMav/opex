# Chat Command Registry — Фаза 2a (handlers-as-commands + web-меню) — план

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** File handlers становятся командами чата (`/summarize_video <url>`, `/transcribe` на вложение), исполняются детерминированно в обход LLM через `insert_handler_job`; недостающий источник/выбор показывается интерактивным меню; `/help`+`/commands` регенерируются из реестра локализованно.

**Architecture:** `HandlerCommandSource` деривирует `CommandSpec` из живых манифестов `HandlerRegistry` (имя=id обработчика, arg `source`, валвсы→choice-арги). Мерж-реестр строится по требованию (builtin ⊕ handler, приоритет builtin) для `/api/commands` и `/help`. Диспетч handler-команд — новый модуль, вызываемый из engine-обёртки `handle_command` ПОСЛЕ того как builtin-`match` вернул `None`: резолв источника (арг → вложение → недавний upload) + трастовый гейт `match_*` + enqueue. `CommandOutcome::Menu` наконец эмитится как `StreamEvent::RichCard` (Фаза 1 оставила фолбэк).

**Tech Stack:** Rust 2024 (opex-core, opex-db), Axum, sqlx, serde; Next.js/React (ui). rustls only.

**Спек:** [2026-07-09-chat-commands-registry-design.md](2026-07-09-chat-commands-registry-design.md) (F1/F2/F4/F5/F6/F7/F8). **Фаза 1 задеплоена** (origin/master 17fb5fab): `agent/commands/{spec,registry,builtin,mod}.rs`, `COMMAND_REGISTRY` LazyLock, `CommandOutcome{Text|Menu}`, `GET /api/commands`, web `CommandAutocomplete`. Эта фаза — 2a; Telegram/Discord — 2b/Фаза 3.

## Global Constraints

- Rust 2024; rustls only, no OpenSSL; no new `.env` keys.
- Имена команд/алиасов: `[a-zA-Z0-9_-]`. Handler-командам имя = id обработчика (уже `[a-z0-9_-]`, валидируется toolgate).
- **Приоритет builtin:** имя/алиас handler-команды НИКОГДА не перекрывает builtin (`/status`, `/new`, …). Конфликт → handler-команда отбрасывается с `tracing::warn!`.
- **Трастовый гейт обязателен:** перед enqueue handler ДОЛЖЕН быть в matched-множестве (`match_buttons` для upload / `match_url_handlers` для url) — mime/домен/allowlist/execution=async. Тот же барьер, что у тула `file_handler` ([file_handler.rs](../../crates/opex-core/src/agent/tool_handlers/file_handler.rs)).
- Только async-обработчики (`execution == "async"`) становятся командами (sync странслируются, у них нет `/complete`-колбэка).
- Rust-тесты авторитетны на сервере (`--bin opex-core`, НЕ `--lib`) с `CARGO_BUILD_JOBS=4 nice ionice` детачед (см. reference_server_rust_test_gotcha). Локально — `cargo check --all-targets -p opex-core`. UI vitest — только из `ui/`.
- Коммиты: 1/задача, без `Co-Authored-By`, master, push/deploy только с явного разрешения.

## Подтверждённые интерфейсы Фазы 1 / существующего кода

- `crate::agent::commands::spec::{CommandSpec, CommandArg, Choice, Choices, CommandScope, CommandCategory, ArgType, Visibility, CommandSourceKind, CommandOutcome, sanitize_native_name}`.
- `crate::agent::commands::registry::{CommandRegistry, CommandSource}` — `from_sources`, `resolve`, `visible_for`, `all`. `COMMAND_REGISTRY: LazyLock<CommandRegistry>` в `commands/mod.rs`.
- `crate::agent::handler_registry::{HandlerRegistry, HandlerManifest, HandlerButton, match_buttons, match_url_handlers, retain_async_handlers}`. `HandlerManifest { id, labels: HashMap<String,String>, descriptions: HashMap<String,String>, match_: HandlerMatch, execution: String, config: serde_json::Value (массив {name,type,default,label,description}), ... }`. `HandlerRegistry::new(toolgate_url, http)`, `.refresh().await`, `.manifests().await -> Vec<HandlerManifest>`.
- `opex_db::handler_jobs::insert_handler_job(db, upload_id: Option<Uuid>, source_ref: Option<&str>, handler_id: &str, agent_name: &str, session_id: Uuid, params: &Value) -> Result<Uuid>`.
- `crate::agent::fse::get_enabled_allowlist(db).await` — allowlist для builtin-tier.
- `uploads`: `owner_type='client_upload'`, `owner_id` = UUID сообщения как строка ([052_uploads_table.sql](../../migrations/052_uploads_table.sql)); `crate::db::uploads::get_by_id(db, uuid)`.
- Pipeline: `PipelineEvent::Stream(StreamEvent::RichCard { card_type: String, data: serde_json::Value })` — ловится `ChannelStatusSink` ([sink.rs:142](../../crates/opex-core/src/agent/pipeline/sink.rs)) только для `"handler_menu"`; SSE-конвертер маппит через `SseEvent::from` ([opex-types/src/sse.rs](../../crates/opex-types/src/sse.rs)). `crate::agent::engine::RICH_CARD_PREFIX`.
- Engine-обёртка `AgentEngine::handle_command` ([context_builder.rs:171](../../crates/opex-core/src/agent/engine/context_builder.rs)) → `commands::handle_command`, возвращает `Option<Result<CommandOutcome>>`. Engine достаёт toolgate-зависимости так же, как для тула `file_handler` (см. `ToolDeps` construction: `toolgate_url`, `http_client`, `db`, `agent_name`, `session_id`).

---

## Файловая структура (2a)

**Создаётся:**
- `crates/opex-core/src/agent/commands/handler_source.rs` — `derive_handler_commands(manifests, enabled, lang) -> Vec<CommandSpec>` + `HandlerCommandSource`.
- `crates/opex-core/src/agent/commands/dispatch.rs` — `try_handler_command(...)`: резолв источника + трастовый гейт + enqueue/menu.
- `crates/opex-core/src/agent/commands/merge.rs` — `build_registry(manifests, enabled, lang) -> CommandRegistry` (builtin ⊕ handler, приоритет builtin, отброс конфликтов).
- `ui/src/components/chat/command-args-menu-card.tsx` — rich-card рендер меню аргументов.

**Модифицируется:**
- `crates/opex-core/src/agent/commands/mod.rs` — `pub mod handler_source; pub mod dispatch; pub mod merge;`.
- `crates/opex-core/src/agent/commands/spec.rs` — `render_help` (локализованные заголовки).
- `crates/opex-core/src/agent/commands/builtin.rs` — локализованные описания через `localization`.
- `crates/opex-core/src/agent/pipeline/commands.rs` — `/help`,`/commands` регенерируются из реестра; `CommandContext` +поля (см. Task 7).
- `crates/opex-core/src/agent/engine/context_builder.rs` — обёртка: после builtin-`None` пробует `try_handler_command`.
- `crates/opex-core/src/agent/engine/run.rs:173,400,585` — `CommandOutcome::Menu` → `StreamEvent::RichCard`.
- `crates/opex-core/src/agent/pipeline/sink.rs:144` — ловить и `command_args_menu`.
- `crates/opex-core/src/gateway/handlers/commands.rs` — `/api/commands` мержит handler-команды (per-lang, ETag/version F8).
- `crates/opex-core/src/gateway/handlers/files.rs` — обобщить `menu_run_core` под завершение команды (или новый `/api/commands/run`).
- `ui/src/components/chat/card-registry.tsx` — регистрация `command_args_menu`.

---

## Task 1: `derive_handler_commands` — деривация CommandSpec из манифестов

**Files:**
- Create: `crates/opex-core/src/agent/commands/handler_source.rs`
- Modify: `crates/opex-core/src/agent/commands/mod.rs` (`pub mod handler_source;`)
- Test: инлайн в `handler_source.rs`

**Interfaces:**
- Consumes: `HandlerManifest` (handler_registry), `CommandSpec`/`CommandArg`/`Choices`/`Choice`/`CommandCategory`/`CommandScope`/`Visibility`/`CommandSourceKind`/`ArgType`/`sanitize_native_name` (spec).
- Produces:
  - `fn derive_handler_commands(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> Vec<CommandSpec>`
  - `struct HandlerCommandSource { specs: Vec<CommandSpec> }` impl `CommandSource` (holds pre-derived specs).

**Правила деривации (только `execution=="async"`):** name = id обработчика; description = `descriptions[lang]` → `descriptions["en"]` → `id`; один positional arg `source` (`capture_remaining`, `required:false`, `menu:false`); для каждого валва из `config`-массива с непустыми `choices`/`enum` — опциональный named arg с `Choices::Static` (валвы без enum → `choices:None`); `visibility: All`; `scope: Both`; `source: Handler { handler_id: id }`; category `Media`. Builtin-tier обработчики (`tier=="builtin"`) включаются только если id ∈ `enabled`.

- [ ] **Step 1: Написать падающий тест**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::handler_registry::HandlerManifest;
    use serde_json::json;

    fn manifest(id: &str, exec: &str, tier: &str) -> HandlerManifest {
        serde_json::from_value(json!({
            "id": id, "execution": exec, "tier": tier,
            "descriptions": {"en": format!("{id} desc"), "ru": format!("{id} описание")},
            "config": []
        })).unwrap()
    }

    #[test]
    fn derives_async_handler_with_source_arg_and_lang_desc() {
        let m = vec![manifest("summarize_video", "async", "workspace")];
        let specs = derive_handler_commands(&m, &[], "ru");
        assert_eq!(specs.len(), 1);
        let c = &specs[0];
        assert_eq!(c.name, "summarize_video");
        assert_eq!(c.description, "summarize_video описание");
        assert_eq!(c.args.len(), 1);
        assert_eq!(c.args[0].name, "source");
        assert!(c.args[0].capture_remaining);
        assert!(matches!(c.source, CommandSourceKind::Handler { .. }));
    }

    #[test]
    fn skips_sync_handlers() {
        let m = vec![manifest("describe", "sync", "workspace")];
        assert!(derive_handler_commands(&m, &[], "en").is_empty());
    }

    #[test]
    fn builtin_tier_gated_by_allowlist() {
        let m = vec![manifest("transcribe", "async", "builtin")];
        assert!(derive_handler_commands(&m, &[], "en").is_empty(), "not in allowlist");
        assert_eq!(derive_handler_commands(&m, &["transcribe".into()], "en").len(), 1);
    }
}
```

- [ ] **Step 2: Прогнать — падает**

Run: `cargo test -p opex-core commands::handler_source -- --nocapture`
Expected: FAIL — `derive_handler_commands` не определён.

- [ ] **Step 3: Реализовать**

`crates/opex-core/src/agent/commands/handler_source.rs`:

```rust
use super::registry::CommandSource;
use super::spec::*;
use crate::agent::handler_registry::HandlerManifest;

fn desc_for<'a>(m: &'a HandlerManifest, lang: &str) -> String {
    m.descriptions.get(lang)
        .or_else(|| m.descriptions.get("en"))
        .cloned()
        .unwrap_or_else(|| m.id.clone())
}

/// Optional named args from valve (`config`) fields that declare enum choices.
fn valve_args(config: &serde_json::Value, lang: &str) -> Vec<CommandArg> {
    let Some(arr) = config.as_array() else { return vec![] };
    arr.iter().filter_map(|f| {
        let name = f.get("name")?.as_str()?.to_string();
        let choices = f.get("choices").or_else(|| f.get("enum"))
            .and_then(|c| c.as_array())
            .map(|vs| Choices::Static {
                values: vs.iter().filter_map(|v| v.as_str().map(|s| Choice { value: s.into(), label: s.into() })).collect(),
            });
        let description = f.get("label").and_then(|l| l.as_str())
            .map(|s| s.to_string()).unwrap_or_default();
        let _ = lang; // labels are single-locale in v1
        Some(CommandArg {
            name, description, arg_type: ArgType::String, required: false,
            choices, capture_remaining: false, menu: true,
        })
    }).collect()
}

pub fn derive_handler_commands(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> Vec<CommandSpec> {
    manifests.iter()
        .filter(|m| m.execution == "async")
        .filter(|m| m.tier != "builtin" || enabled.iter().any(|e| e == &m.id))
        .map(|m| {
            let mut args = vec![CommandArg {
                name: "source".into(), description: "url or file".into(),
                arg_type: ArgType::String, required: false, choices: None,
                capture_remaining: true, menu: false,
            }];
            args.extend(valve_args(&m.config, lang));
            CommandSpec {
                name: m.id.clone(), aliases: vec![], description: desc_for(m, lang),
                category: CommandCategory::Media, scope: CommandScope::Both, args,
                visibility: Visibility::All,
                source: CommandSourceKind::Handler { handler_id: m.id.clone() },
            }
        })
        .collect()
}

pub struct HandlerCommandSource { specs: Vec<CommandSpec> }
impl HandlerCommandSource {
    pub fn new(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> Self {
        Self { specs: derive_handler_commands(manifests, enabled, lang) }
    }
}
impl CommandSource for HandlerCommandSource {
    fn specs(&self) -> Vec<CommandSpec> { self.specs.clone() }
}
```

Add `pub mod handler_source;` to `commands/mod.rs`.

- [ ] **Step 4: Прогнать — зелёные**

Run: `cargo test -p opex-core commands::handler_source -- --nocapture` → PASS (3).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/handler_source.rs crates/opex-core/src/agent/commands/mod.rs
git commit -m "feat(commands): derive handler-command CommandSpecs from HandlerRegistry manifests"
```

---

## Task 2: `build_registry` — мерж builtin ⊕ handler с приоритетом builtin

**Files:**
- Create: `crates/opex-core/src/agent/commands/merge.rs`
- Modify: `commands/mod.rs` (`pub mod merge;`)
- Test: инлайн в `merge.rs`

**Interfaces:**
- Consumes: `BuiltinCommandSource`, `HandlerCommandSource`, `CommandRegistry::from_sources`, `derive_handler_commands`.
- Produces: `fn build_registry(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> CommandRegistry` — builtins + handler-команды, где имя/алиас handler-команды НЕ дублирует builtin (иначе dropped + warn).

- [ ] **Step 1: Тест приоритета builtin + мержа**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::handler_registry::HandlerManifest;
    use serde_json::json;

    fn m(id: &str) -> HandlerManifest {
        serde_json::from_value(json!({"id": id, "execution":"async", "tier":"workspace",
            "descriptions": {"en": "d"}, "config": []})).unwrap()
    }

    #[test]
    fn handler_merges_but_never_shadows_builtin() {
        // "new" is a builtin; a handler also called "new" must be dropped.
        let reg = build_registry(&[m("summarize_video"), m("new")], &[], "en");
        assert!(reg.resolve("summarize_video").is_some());
        // "new" resolves to the BUILTIN (source kind Builtin), not the handler
        let new_cmd = reg.resolve("new").unwrap();
        assert!(matches!(new_cmd.source, crate::agent::commands::spec::CommandSourceKind::Builtin));
        // registry still validates (no dup names)
        assert!(reg.resolve("status").is_some());
    }
}
```

- [ ] **Step 2: Прогнать — падает**

Run: `cargo test -p opex-core commands::merge -- --nocapture` → FAIL.

- [ ] **Step 3: Реализовать**

`crates/opex-core/src/agent/commands/merge.rs`:

```rust
use super::builtin::BuiltinCommandSource;
use super::handler_source::derive_handler_commands;
use super::registry::{CommandRegistry, CommandSource};
use super::spec::CommandSpec;
use crate::agent::handler_registry::HandlerManifest;
use std::collections::HashSet;

pub fn build_registry(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> CommandRegistry {
    let builtins = BuiltinCommandSource.specs();
    let mut taken: HashSet<String> = HashSet::new();
    for b in &builtins {
        taken.insert(b.name.to_lowercase());
        for a in &b.aliases { taken.insert(a.to_lowercase()); }
    }
    let mut handlers: Vec<CommandSpec> = Vec::new();
    for h in derive_handler_commands(manifests, enabled, lang) {
        if taken.contains(&h.name.to_lowercase())
            || h.aliases.iter().any(|a| taken.contains(&a.to_lowercase())) {
            tracing::warn!(command = %h.name, "handler command dropped — name/alias collides with builtin");
            continue;
        }
        taken.insert(h.name.to_lowercase());
        for a in &h.aliases { taken.insert(a.to_lowercase()); }
        handlers.push(h);
    }
    // Both sources already conflict-free against each other → from_sources validates.
    let merged = MergedSource(builtins.into_iter().chain(handlers).collect());
    CommandRegistry::from_sources(&[&merged]).expect("merged registry must validate")
}

struct MergedSource(Vec<CommandSpec>);
impl CommandSource for MergedSource { fn specs(&self) -> Vec<CommandSpec> { self.0.clone() } }
```

Add `pub mod merge;` to `commands/mod.rs`.

- [ ] **Step 4: Прогнать — зелёные**

Run: `cargo test -p opex-core commands::merge -- --nocapture` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/merge.rs crates/opex-core/src/agent/commands/mod.rs
git commit -m "feat(commands): build_registry merges builtin + handler commands, builtin precedence"
```

---

## Task 3: `/api/commands` мержит handler-команды (per-lang + ETag/version, F8)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/commands.rs`
- Test: инлайн в `commands.rs` (handler)

**Interfaces:**
- Consumes: `build_registry`, `HandlerRegistry`, `get_enabled_allowlist`, `AppState.handlers` (`HandlerRegistry` уже в AppState).
- Produces: `GET /api/commands?agent=&lang=&scope=` возвращает builtin + handler-команды; `version` = ETag `HandlerRegistry` (или счётчик манифестов), не `len`.

- [ ] **Step 1: Тест — handler-команда попадает в мерж**

```rust
#[cfg(test)]
mod tests {
    use crate::agent::commands::merge::build_registry;
    use crate::agent::handler_registry::HandlerManifest;
    use serde_json::json;

    #[test]
    fn merged_registry_serializes_builtin_plus_handler() {
        let m: HandlerManifest = serde_json::from_value(json!({
            "id":"summarize_video","execution":"async","tier":"workspace",
            "descriptions":{"en":"Summarize a video"},"config":[]})).unwrap();
        let reg = build_registry(&[m], &[], "en");
        let json = serde_json::to_value(reg.all()).unwrap();
        let names: Vec<&str> = json.as_array().unwrap().iter()
            .map(|c| c["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"status"));         // builtin
        assert!(names.contains(&"summarize_video")); // handler
    }
}
```

- [ ] **Step 2: Прогнать — падает** (build_registry not used by handler yet)

Run: `cargo test -p opex-core gateway::handlers::commands -- --nocapture` → FAIL/compile.

- [ ] **Step 3: Реализовать — переписать `list_commands`**

Заменить тело `list_commands` в `commands.rs`, взяв `State<AppState>` (нужен `state.handlers` + `state.*.db`). Ключевые строки:

```rust
async fn list_commands(State(state): State<AppState>, Query(q): Query<CommandsQuery>) -> impl IntoResponse {
    let lang = q.lang.as_deref().unwrap_or("en");
    let db = state.db();                       // AppState accessor to PgPool (mirror other handlers)
    let reg_handles = &state.handlers;         // HandlerRegistry
    reg_handles.refresh().await;
    let manifests = reg_handles.manifests().await;
    let enabled = crate::agent::fse::get_enabled_allowlist(db).await;
    let registry = crate::agent::commands::merge::build_registry(&manifests, &enabled, lang);
    let mut specs = registry.visible_for(false);
    if q.scope.as_deref() == Some("native") {
        specs.retain(|c| matches!(c.scope,
            crate::agent::commands::spec::CommandScope::Native | crate::agent::commands::spec::CommandScope::Both));
    }
    let version = reg_handles.etag().await.unwrap_or_else(|| specs.len().to_string()); // F8
    Json(serde_json::json!({ "commands": specs, "version": version }))
}
```

Detail: если `HandlerRegistry` не имеет `etag()` getter — добавить его (возвращает текущий ETag из `refresh`-кэша; там уже хранится ETag для conditional GET). Использовать реальный accessor `AppState`→`PgPool` (как в соседних хендлерах, напр. `state.agents.db` / `state.config…`; найти по образцу).

- [ ] **Step 4: Прогнать — зелёные + сервер-smoke**

Run: `cargo test -p opex-core gateway::handlers::commands merge -- --nocapture` → PASS.
После деплоя: `curl .../api/commands | jq '.commands[].name'` содержит `summarize_video`.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/commands.rs
git commit -m "feat(api): /api/commands merges live handler commands per-lang with ETag version"
```

---

## Task 4: `CommandOutcome::Menu` → `StreamEvent::RichCard` (F1/F7)

**Files:**
- Modify: `crates/opex-core/src/agent/engine/run.rs:173,400,585` (3 блока)
- Modify: `crates/opex-core/src/agent/pipeline/sink.rs:144`
- Test: инлайн в `sink.rs` (ChannelStatusSink ловит `command_args_menu`)

**Interfaces:**
- Consumes: `CommandOutcome::Menu { card }`, `StreamEvent::RichCard { card_type, data }`.
- Produces: Menu-команда эмитит `RichCard` вместо текста; `ChannelStatusSink` захватывает `command_args_menu` в `self.menu`.

- [ ] **Step 1: Тест sink**

Дополнить тесты `sink.rs`:

```rust
#[tokio::test]
async fn channel_sink_captures_command_args_menu() {
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let mut sink = ChannelStatusSink::new(None, Some(tx));
    let card = serde_json::json!({"card_type":"command_args_menu","x":1});
    sink.emit(PipelineEvent::Stream(StreamEvent::RichCard {
        card_type: "command_args_menu".into(), data: card.clone() })).await.unwrap();
    assert_eq!(sink.menu, Some(card));
}
```

- [ ] **Step 2: Прогнать — падает** (only `handler_menu` captured today)

Run: `cargo test -p opex-core pipeline::sink -- --nocapture` → FAIL.

- [ ] **Step 3: Реализовать**

`sink.rs:144` — расширить матч:

```rust
if card_type == "handler_menu" || card_type == "command_args_menu" {
    self.menu = Some(data);
}
```

`run.rs` — в КАЖДОМ из 3 блоков заменить `Menu { card } => card.to_string()`-фолбэк на реальную эмиссию. Блок становится:

```rust
if let Some(outcome) = command_output.take() {
    match outcome {
        CommandOutcome::Menu { card } => {
            let card_type = card.get("card_type").and_then(|v| v.as_str())
                .unwrap_or("command_args_menu").to_string();
            let _ = s.emit(PipelineEvent::Stream(StreamEvent::RichCard { card_type, data: card })).await;
            let _ = s.emit(PipelineEvent::Stream(StreamEvent::Finish {
                finish_reason: "command".to_string(), continuation: false })).await;
            // finalize with empty assistant text (the card IS the response)
            let fin_ctx = finalize::finalize_context_from_engine(
                self, session_id, boot_for_execute.messages.len(),
                Some(user_message_id), compressor, uuid::Uuid::new_v4());
            return finalize::finalize(fin_ctx, finalize::FinalizeOutcome::Done {
                assistant_text: String::new(), thinking_json: None, turn_limited: false,
            }, &mut s, &mut lifecycle_guard).await;
        }
        CommandOutcome::Text(text) => {
            // …existing Text path unchanged (MessageStart/TextDelta/Finish/finalize)…
        }
    }
}
```

(Точная форма Text-ветки — как сейчас в каждом из трёх блоков; сохранить её дословно, обернув в `CommandOutcome::Text(text) => { … }`. SSE-блок (173) шлёт `MessageStart` перед RichCard; канальный (400) и chunk (585) — без MessageStart, как их текущая Text-ветка.)

- [ ] **Step 4: Прогнать + компиляция**

Run: `cargo test -p opex-core pipeline::sink -- --nocapture && cargo check -p opex-core --all-targets` → PASS + чисто.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/engine/run.rs crates/opex-core/src/agent/pipeline/sink.rs
git commit -m "feat(commands): emit CommandOutcome::Menu as RichCard (command_args_menu)"
```

---

## Task 5: `try_handler_command` — резолв источника + трастовый гейт + enqueue

**Files:**
- Create: `crates/opex-core/src/agent/commands/dispatch.rs`
- Modify: `commands/mod.rs` (`pub mod dispatch;`), `context_builder.rs` (engine-обёртка)
- Test: инлайн unit для парсинга + резолва (чистые части)

**Interfaces:**
- Consumes: `HandlerRegistry` (или `manifests`+`match_buttons`/`match_url_handlers`+`retain_async_handlers`), `insert_handler_job`, `db::uploads::get_by_id`, `get_enabled_allowlist`, `CommandOutcome`.
- Produces:
  - `struct HandlerDispatchDeps<'a> { db, toolgate_url: &str, http: reqwest::Client, agent_name, agent_language, session_id: Uuid }`
  - `async fn try_handler_command(deps, text, msg) -> Option<Result<CommandOutcome>>` — `None` если `/name` не матчит ни одну async handler-команду; иначе `Some`.
  - helper `fn parse_command_line(text) -> Option<(String, String)>` (name без `/`, остаток-args).

**Резолв источника** (F4): (1) `args`-строка если непустая и выглядит как url/путь → `source_ref`; (2) `msg.attachments` первый upload → `upload_id`; (3) недавний upload сессии — `SELECT id FROM uploads WHERE owner_type='client_upload' AND owner_id IN (SELECT id::text FROM messages WHERE session_id=$1) ORDER BY created_at DESC LIMIT 1`; (4) нет источника → `CommandOutcome::Menu` с card `command_args_menu` (список кандидатов/просьба прислать url/файл). **Трастовый гейт** (F6): resolved handler ДОЛЖЕН быть в `match_url_handlers(manifests,url,enabled,lang)` (для url) или `match_buttons`+`retain_async_handlers` (для upload mime) — иначе `Some(Ok(Text("недоступен для этого источника")))`.

- [ ] **Step 1: Тест парсера + маршрутизации None**

```rust
#[cfg(test)]
mod tests {
    use super::parse_command_line;
    #[test]
    fn parses_name_and_args() {
        assert_eq!(parse_command_line("/summarize_video https://x/y"),
            Some(("summarize_video".into(), "https://x/y".into())));
        assert_eq!(parse_command_line("/transcribe"), Some(("transcribe".into(), "".into())));
        assert_eq!(parse_command_line("no slash"), None);
    }
}
```

- [ ] **Step 2: Прогнать — падает**

Run: `cargo test -p opex-core commands::dispatch -- --nocapture` → FAIL.

- [ ] **Step 3: Реализовать `dispatch.rs`** (структура — резолв/гейт/enqueue; парсер тестируется юнитом; полный путь — E2E на сервере, т.к. нужен toolgate)

```rust
use super::spec::CommandOutcome;
use crate::agent::handler_registry::{HandlerRegistry, match_buttons, match_url_handlers, retain_async_handlers};
use anyhow::Result;
use opex_types::IncomingMessage;
use uuid::Uuid;

pub struct HandlerDispatchDeps<'a> {
    pub db: &'a sqlx::PgPool,
    pub toolgate_url: String,
    pub http: reqwest::Client,
    pub agent_name: &'a str,
    pub agent_language: &'a str,
    pub session_id: Uuid,
}

pub fn parse_command_line(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let rest = t.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if name.is_empty() { return None; }
    Some((name.split('@').next().unwrap_or(name).to_lowercase(), args.trim().to_string()))
}

pub async fn try_handler_command(
    deps: &HandlerDispatchDeps<'_>, text: &str, msg: &IncomingMessage,
) -> Option<Result<CommandOutcome>> {
    let (name, args) = parse_command_line(text)?;
    let reg = HandlerRegistry::new(deps.toolgate_url.clone(), deps.http.clone());
    reg.refresh().await;
    let manifests = reg.manifests().await;
    // Only async handlers become commands; require the name to match one.
    if !manifests.iter().any(|m| m.execution == "async" && m.id == name) {
        return None; // not a handler command → caller passes to LLM
    }
    let enabled = crate::agent::fse::get_enabled_allowlist(deps.db).await;
    let lang = deps.agent_language;

    // Source resolution: explicit url arg → attachment → recent upload → menu.
    let url_arg = (!args.is_empty()).then(|| args.clone());
    let upload_id = resolve_upload(deps, msg).await; // Option<Uuid> — attachment or recent session upload

    let (upload, source_ref, gated_ok) = if let Some(u) = &url_arg {
        let ok = match_url_handlers(&manifests, u, &enabled, lang).iter().any(|b| b.id == name);
        (None, Some(u.clone()), ok)
    } else if let Some(uid) = upload_id {
        let row = crate::db::uploads::get_by_id(deps.db, uid).await.ok().flatten();
        match row {
            Some(r) => {
                let mut b = match_buttons(&manifests, &r.mime, r.size_bytes.max(0) as u64, &enabled, lang);
                retain_async_handlers(&mut b, &manifests);
                (Some(uid), None, b.iter().any(|x| x.id == name))
            }
            None => (None, None, false),
        }
    } else {
        // No source → argsMenu (Phase 2b/Telegram renders buttons; web renders card).
        let card = serde_json::json!({
            "card_type": "command_args_menu", "command": name,
            "text": format!("Пришлите ссылку или файл для /{name}."),
        });
        return Some(Ok(CommandOutcome::Menu { card }));
    };

    if !gated_ok {
        return Some(Ok(CommandOutcome::Text(format!(
            "Обработчик `{name}` недоступен для этого источника."))));
    }
    let params = serde_json::json!({ "language": lang });
    match opex_db::handler_jobs::insert_handler_job(
        deps.db, upload, source_ref.as_deref(), &name, deps.agent_name, deps.session_id, &params,
    ).await {
        Ok(_) => Some(Ok(CommandOutcome::Text(format!(
            "✅ Запустил `/{name}`. Результат придёт в чат по готовности.")))),
        Err(e) => Some(Ok(CommandOutcome::Text(format!("Ошибка постановки задачи: {e}")))),
    }
}

async fn resolve_upload(deps: &HandlerDispatchDeps<'_>, msg: &IncomingMessage) -> Option<Uuid> {
    // 1) attachment on the current message (msg.attachments carry upload ids/urls — mirror enrich path)
    if let Some(id) = msg.attachments.iter().find_map(|a| a.upload_id) { return Some(id); }
    // 2) most-recent client_upload in this session (uploads.owner_id = message uuid string)
    sqlx::query_scalar::<_, Uuid>(
        "SELECT u.id FROM uploads u \
         WHERE u.owner_type='client_upload' \
           AND u.owner_id IN (SELECT m.id::text FROM messages m WHERE m.session_id=$1) \
         ORDER BY u.created_at DESC LIMIT 1")
        .bind(deps.session_id).fetch_optional(deps.db).await.ok().flatten()
}
```

(Note: `msg.attachments`'s exact upload-id accessor — mirror how bootstrap enrich reads attachments; if attachments carry a URL not an upload id, treat as `source_ref` instead. Confirm the `Attachment` struct shape and adapt this one accessor.)

Add `pub mod dispatch;` to `commands/mod.rs`. Wire into engine-обёртка `context_builder.rs::handle_command`: after `commands::handle_command(...)` returns `None` AND `text.trim().starts_with('/')`, build `HandlerDispatchDeps` from `self.cfg()` (same toolgate_url/http/db/agent/session as the `file_handler` tool's ToolDeps) and `return dispatch::try_handler_command(&deps, text, msg).await;`. Session id: resolve the active session for this chat (as builtins do via `find_active_session`) or thread it from the pipeline; if unavailable, skip (return None).

- [ ] **Step 4: Прогнать unit + компиляция**

Run: `cargo test -p opex-core commands::dispatch -- --nocapture && cargo check -p opex-core --all-targets` → PASS + чисто.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/dispatch.rs crates/opex-core/src/agent/commands/mod.rs crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "feat(commands): dispatch handler commands (source resolve + trust gate + enqueue)"
```

---

## Task 6: web `command_args_menu` rich-card

**Files:**
- Create: `ui/src/components/chat/command-args-menu-card.tsx`
- Modify: `ui/src/components/chat/card-registry.tsx`
- Test: `ui/src/components/chat/command-args-menu-card.test.tsx`

**Interfaces:**
- Consumes: rich-card `{ card_type:"command_args_menu", command, text, options? }` из SSE.
- Produces: карточка с текстом + (если есть `options`) кнопки, постящие в run-эндпоинт (Task 7 backend / уже menu-run). В 2a MVP: рендер текста-подсказки; кнопки выбора значений — когда `options` присутствуют.

- [ ] **Step 1: Падающий vitest** (из `ui/`)

```tsx
import { render, screen } from "@testing-library/react";
import { CommandArgsMenuCard } from "./command-args-menu-card";

it("renders the prompt text", () => {
  render(<CommandArgsMenuCard data={{ card_type:"command_args_menu", command:"summarize_video", text:"Пришлите ссылку" }} />);
  expect(screen.getByText(/Пришлите ссылку/)).toBeInTheDocument();
});
```

- [ ] **Step 2: Прогнать — падает**

Run (из `ui/`): `npm test -- command-args-menu-card` → FAIL.

- [ ] **Step 3: Реализовать компонент + регистрация**

`ui/src/components/chat/command-args-menu-card.tsx`:

```tsx
interface Data { card_type: string; command?: string; text?: string; options?: { value: string; label: string }[]; }
export function CommandArgsMenuCard({ data }: { data: Data }) {
  return (
    <div className="rounded-md border bg-card p-3">
      {data.text && <p className="text-sm">{data.text}</p>}
      {data.options?.length ? (
        <div className="mt-2 flex flex-wrap gap-2">
          {data.options.map((o) => (
            <button key={o.value} type="button" className="rounded-md border px-2 py-1 text-xs hover:bg-accent">
              {o.label}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
```

Register in `card-registry.tsx`: add `command_args_menu: CommandArgsMenuCard` to the `CARD_REGISTRY` map (mirror the existing `handler_menu` entry).

- [ ] **Step 4: Прогнать + build**

Run (из `ui/`): `npm test -- command-args-menu-card` → PASS; `npm run build` → clean.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/chat/command-args-menu-card.tsx ui/src/components/chat/command-args-menu-card.test.tsx ui/src/components/chat/card-registry.tsx
git commit -m "feat(ui): command_args_menu rich-card renderer"
```

---

## Task 7: Локализованный `/help` + `/commands` из мерж-реестра (P2)

**Files:**
- Modify: `crates/opex-core/src/agent/commands/spec.rs` (`render_help` — заголовки категорий из `localization`)
- Modify: `crates/opex-core/src/agent/commands/builtin.rs` (описания через `localization`)
- Modify: `crates/opex-core/src/agent/pipeline/commands.rs` (`/help`,`/commands` строят мерж-реестр и рендерят; `CommandContext` +`toolgate`/`http` для мержа, или строить builtin-only если toolgate недоступен)
- Modify: `crates/opex-core/src/agent/localization.rs` (+строки категорий)
- Test: инлайн (render_help содержит локализованные заголовки + handler-команды)

**Interfaces:**
- Consumes: `build_registry`, `localization::get_strings`.
- Produces: `/help` рендерит builtin + handler-команды, сгруппированные по категориям с локализованными заголовками; описания builtin локализованы.

- [ ] **Step 1: Тест**

```rust
#[test]
fn help_localized_headers_and_includes_handlers() {
    // build a merged registry with one handler, render RU help
    use crate::agent::handler_registry::HandlerManifest;
    let m: HandlerManifest = serde_json::from_value(serde_json::json!({
        "id":"summarize_video","execution":"async","tier":"workspace",
        "descriptions":{"ru":"Конспект видео"},"config":[]})).unwrap();
    let reg = crate::agent::commands::merge::build_registry(&[m], &[], "ru");
    let visible = reg.visible_for(false);
    let out = render_help(&visible, "ru");
    assert!(out.contains("summarize_video"));
    assert!(out.contains("Конспект видео"));
}
```

- [ ] **Step 2: Прогнать — падает** (render_help currently hardcodes RU literals; builtin descriptions are EN)

Run: `cargo test -p opex-core commands::spec::tests::help_localized -- --nocapture` → FAIL/assert.

- [ ] **Step 3: Реализовать**

- В `localization.rs`: добавить в `Strings` поля `cat_session/cat_options/cat_status/cat_management/cat_media/cat_tools` (RU/EN значения).
- `render_help(specs, lang)`: заголовки категорий из `localization::get_strings(lang)` вместо литералов.
- `builtin.rs`: заменить статичные EN-`description` на `localization`-ключи (либо оставить EN для `/api/commands` v1 и локализовать ТОЛЬКО в `render_help` через отдельную таблицу `builtin_desc(name, lang)`). **Решение:** `/api/commands` оставляет EN-описания (v1, web-tooltip); `/help` использует локализованную таблицу описаний builtin из `localization`. Это разъединяет API-описания и help-описания без ломки Фазы-1 API-контракта.
- `commands.rs` `"/help" | "/commands"`: построить мерж-реестр (`build_registry(manifests, enabled, lang)` — нужны manifests: получить через `CommandContext`-переданный `HandlerRegistry`/toolgate; если недоступно — builtin-only через `COMMAND_REGISTRY.all()`), `render_help(&visible, ctx.agent_language)`. Добавить в `CommandContext` опциональные `toolgate_url`/`http` (заполняются engine-обёрткой; при `None` — builtin-only).

- [ ] **Step 4: Прогнать + компиляция**

Run: `cargo test -p opex-core commands:: -- --nocapture && cargo check -p opex-core --all-targets` → PASS + чисто.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/spec.rs crates/opex-core/src/agent/commands/builtin.rs crates/opex-core/src/agent/pipeline/commands.rs crates/opex-core/src/agent/localization.rs
git commit -m "feat(commands): registry-generated localized /help + /commands incl. handler commands"
```

---

## Task 8: Интеграция, серверный тест, деплой, E2E

**Files:** нет новых.

- [ ] **Step 1: Серверный тест-пасс (bounded)**

bundle→worktree, `CARGO_BUILD_JOBS=4 nice -n 19 ionice -c3` детачед, `cargo test -p opex-core --bin opex-core commands -- --nocapture`. Ожидание: все команд-тесты (Фаза 1 + новые handler_source/merge/dispatch/sink/spec) зелёные. (Локально предварительно `cargo check --all-targets`.)

- [ ] **Step 2: UI**

Run (из `ui/`): `npm test` (полный, зелёный) + `npm run build`.

- [ ] **Step 3: Деплой (с разрешения пользователя)**

Push origin (ff) → Rust `bash ~/opex-src/scripts/server-deploy.sh` → UI `bash scripts/deploy-ui.sh`. Проверить `curl /api/commands | jq '.commands[].name'` содержит handler-команды; doctor 200.

- [ ] **Step 4: E2E на web**

`/summarize_video <youtube-url>` в web-чате → ставит `handler_job`, приходит «✅ Запустил», по готовности — результат. `/summarize_video` без url → `command_args_menu` карточка. Автодополнение `/` показывает handler-команды рядом с builtin.

- [ ] **Step 5: Маркер завершения 2a**

```bash
git commit --allow-empty -m "chore(commands): Phase 2a complete — handlers-as-commands + web args-menu + localized /help"
```

---

## Не в объёме 2a (→ 2b / Фаза 3)

- Telegram `setMyCommands` из `?scope=native`, выпил статик-списка telegram.ts, channel-side `cmd*`-строк (F2/F5) — Фаза 2b.
- Telegram inline-callback argsMenu через `MENU_CTX`, обобщение `menu_run_core` под завершение команды — 2b.
- `<command>`-оверрайд в toolgate-дескрипторе (кастомное имя/алиасы/арги) — минорное расширение, 2b или позже; 2a использует авто-деривацию.
- Discord — Фаза 3.
- Choice-args argsMenu на web с реальным run-по-клику (2a рендерит текст-подсказку; кнопки-значения — когда handler объявляет valve-choices; полный клик→run — 2b вместе с обобщённым endpoint).

## Self-Review (2a)

- **Покрытие outline Фазы 2 (часть 2a):** HandlerCommandSource+деривация (T1 ✓), мерж+builtin-приоритет (T2 ✓), `/api/commands`+handler+ETag F8 (T3 ✓), Menu→RichCard F1/F7 (T4 ✓), диспетч+резолв источника F4+трастовый гейт F6 (T5 ✓), web card (T6 ✓), локализованный /help P2 (T7 ✓), E2E (T8 ✓). Отложено в 2b: Telegram F2/F5, обобщённый run-endpoint + click-to-run, `<command>`-оверрайд.
- **Плейсхолдеры:** два места требуют подтверждения точного accessor'а у имплементера (НЕ vague — конкретные): (a) `AppState`→`PgPool` в T3 (по образцу соседних хендлеров); (b) `msg.attachments` upload-id/url shape в T5 (по образцу bootstrap-enrich). Оба — «сверь один accessor», не «додумай логику».
- **Согласованность типов:** `derive_handler_commands`/`build_registry`/`HandlerDispatchDeps`/`try_handler_command`/`parse_command_line`/`render_help` — имена согласованы между задачами; `CommandOutcome::Menu{card}` эмиссия совпадает с card-type `command_args_menu` в sink+web.
