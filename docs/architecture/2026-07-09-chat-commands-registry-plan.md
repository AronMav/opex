# Единый реестр команд чата — план реализации (Фаза 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ввести декларативный `CommandRegistry` в ядре (единый источник истины о командах), отдать список через `GET /api/commands` и включить автодополнение `/` в web-композере — с полным паритетом поведения 14 существующих команд.

**Architecture:** Реестр builtins статичен, поэтому в Фазе 1 живёт как `LazyLock<CommandRegistry>`-синглтон (без проводки через `AppState`/`EngineConfig`). Дескрипторы агрегируются из `CommandSource` (в Фазе 1 — только `BuiltinCommandSource`). Диспетч slash-команд **остаётся текущим `match`** — реестр в Фазе 1 не исполняет, а только описывает (питает `/api/commands` + автодополнение) и валидируется. `/help` и `/commands` остаются локализованными как сегодня; их регенерация из реестра — Фаза 2. Контракт вывода расширяется до `CommandOutcome { Text | Menu }` (вариант `Menu` задействуется в Фазе 2).

**Tech Stack:** Rust 2024 (opex-core), Axum 0.8, serde; Next.js 16 / React 19 / React Query 5 / Zustand (ui). Только rustls, без OpenSSL.

**Спек:** [2026-07-09-chat-commands-registry-design.md](2026-07-09-chat-commands-registry-design.md) (F1–F10 учтены). **Ревью-фиксы плана:** P1 (LazyLock), P2 (`/help` без изменений в Фазе 1), P3 (все команды `Visibility::All` — чистый паритет), P4 (гард дрейфа), P5 (`apiGet`).

## Global Constraints

- Rust 2024 edition; только `rustls-tls`, никакого OpenSSL. Никаких новых ключей в `.env`.
- Имена команд/алиасов: `[a-zA-Z0-9_-]` (анти-traversal инвариант, как у tool/MCP-имён). `nativeName` дополнительно: `[a-z0-9_]{1,32}`.
- **Паритет — приоритет Фазы 1.** Все 14 команд (`/status`, `/new`, `/reset`, `/compact`, `/rollback`, `/model`, `/think`, `/voice`, `/usage`, `/export`, `/help`, `/memory`, `/goal`, `/subgoal`) ведут себя байт-в-байт как сегодня. Все — `Visibility::All` (base-гейтинг НЕ вводим в Фазе 1).
- Тесты Rust авторитетно гоняются на сервере (`make test-db`); локальный Windows Rust-тест ненадёжен. CI: `cargo test --workspace` + tsc + `make gen-types` drift.
- Билд на сервере (`make remote-deploy`); деплой только на 188.x. UI — отдельным `scripts/deploy-ui.sh` абсолютным путём.
- Коммиты: 1 на задачу, без `Co-Authored-By`, работа прямо в `master`. Push — только с явного разрешения пользователя.

---

## Файловая структура (Фаза 1)

**Создаётся:**
- `crates/opex-core/src/agent/commands/mod.rs` — реэкспорт + `LazyLock`-синглтон `command_registry()`.
- `crates/opex-core/src/agent/commands/spec.rs` — типы `CommandSpec`, `CommandArg`, enum-ы, `CommandOutcome`, `validate_registry`, `sanitize_native_name`.
- `crates/opex-core/src/agent/commands/registry.rs` — `CommandRegistry`, трейт `CommandSource`, резолв/фильтрация.
- `crates/opex-core/src/agent/commands/builtin.rs` — `BuiltinCommandSource`: дескрипторы 14 команд + `BUILTIN_NAMES`.
- `crates/opex-core/src/gateway/handlers/commands.rs` — `GET /api/commands`.
- `ui/src/hooks/use-commands.ts` — фетч `/api/commands` (React Query).
- `ui/src/components/chat/command-autocomplete.tsx` — выпадашка автодополнения.
- `ui/src/components/chat/command-autocomplete.test.tsx` — vitest.

**Модифицируется:**
- `crates/opex-core/src/agent/pipeline/commands.rs` — тела `match` НЕ трогаем; меняем только тип успеха `String` → `CommandOutcome::Text`; добавляем `const DISPATCH_NAMES` + drift-гард-тест.
- `crates/opex-core/src/agent/engine/context_builder.rs:170` — обёртка возвращает `CommandOutcome`.
- `crates/opex-core/src/agent/pipeline/bootstrap.rs:29,351-354` — `command_output: Option<CommandOutcome>`.
- `crates/opex-core/src/agent/engine/run.rs:172,394,574` — три early-exit блока разбирают `CommandOutcome::{Text,Menu}`.
- `crates/opex-core/src/agent/mod.rs` — `pub mod commands;`.
- `crates/opex-core/src/gateway/handlers/mod.rs` — `mod commands;` + `.merge(commands::routes())`.
- `ui/src/components/chat/` composer — интеграция автодополнения.
- `ui/src/types/api.ts` — тип `CommandInfo` (hand-mirror gateway-DTO, как `AgentInfo`; ts-rs НЕ участвует — см. P7).

> **P1:** `AppState`, `EngineConfig`, `CommandContext` НЕ меняются — реестр берётся из `LazyLock`-синглтона. **P2:** `/help`/`/commands` в Фазе 1 не трогаем. **P6:** хендлер `/api/commands` извлекает `State<AppState>` напрямую (роутер-стейт = `AppState`) — допустимо; sub-state-извлечение не требуется, т.к. реестр берётся из синглтона, а не из стейта.

---

## Task 1: Типы `CommandSpec` + `CommandOutcome` + валидация + санитайзер

**Files:**
- Create: `crates/opex-core/src/agent/commands/mod.rs`
- Create: `crates/opex-core/src/agent/commands/spec.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (добавить `pub mod commands;`)
- Test: инлайн `#[cfg(test)]` в `spec.rs`

**Interfaces:**
- Produces:
  - `enum CommandScope { Text, Native, Both }`
  - `enum CommandCategory { Session, Options, Status, Management, Media, Tools }` (derive `PartialEq`)
  - `enum ArgType { String, Number, Boolean }`
  - `enum Choices { Static { values: Vec<Choice> }, Dynamic { provider: String } }`, `struct Choice { value, label: String }`
  - `enum Visibility { All, BaseOnly }` (derive `PartialEq`)
  - `enum CommandSourceKind { Builtin, Handler { handler_id: String } }`
  - `struct CommandArg { name, description: String, arg_type: ArgType, required: bool, choices: Option<Choices>, capture_remaining: bool, menu: bool }`
  - `struct CommandSpec { name, aliases, description, category, scope, args, visibility, source }` (все derive `Serialize`)
  - `enum CommandOutcome { Text(String), Menu { card: serde_json::Value } }`
  - `fn validate_registry(specs: &[CommandSpec]) -> Result<(), String>`
  - `fn sanitize_native_name(name: &str) -> Option<String>`

- [ ] **Step 1: Написать падающий тест валидатора и санитайзера**

В `crates/opex-core/src/agent/commands/spec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, scope: CommandScope) -> CommandSpec {
        CommandSpec {
            name: name.to_string(), aliases: vec![], description: "d".into(),
            category: CommandCategory::Status, scope, args: vec![],
            visibility: Visibility::All, source: CommandSourceKind::Builtin,
        }
    }

    #[test]
    fn duplicate_names_rejected() {
        assert!(validate_registry(&[spec("status", CommandScope::Both), spec("status", CommandScope::Text)]).is_err());
    }

    #[test]
    fn duplicate_alias_rejected() {
        let mut a = spec("status", CommandScope::Text); a.aliases = vec!["st".into()];
        let mut b = spec("start", CommandScope::Text); b.aliases = vec!["st".into()];
        assert!(validate_registry(&[a, b]).is_err());
    }

    #[test]
    fn valid_registry_ok() {
        assert!(validate_registry(&[spec("status", CommandScope::Both), spec("new", CommandScope::Text)]).is_ok());
    }

    #[test]
    fn native_name_sanitized() {
        assert_eq!(sanitize_native_name("export-session").as_deref(), Some("export_session"));
        assert_eq!(sanitize_native_name("Summarize_Video").as_deref(), Some("summarize_video"));
        assert_eq!(sanitize_native_name("---").as_deref(), None);
    }
}
```

- [ ] **Step 2: Прогнать — падает**

Run (сервер): `cargo test -p opex-core commands::spec -- --nocapture`
Expected: FAIL — типы/функции не определены.

- [ ] **Step 3: Реализовать типы + валидатор + санитайзер**

`crates/opex-core/src/agent/commands/mod.rs`:

```rust
//! Единый реестр команд чата (спек 2026-07-09).
use std::sync::LazyLock;

pub mod spec;
pub mod registry;
pub mod builtin;

/// Синглтон реестра builtins. Валидируется при первом обращении;
/// панике при невалидности — конфигурация команд статична и обязана быть корректной.
pub static COMMAND_REGISTRY: LazyLock<registry::CommandRegistry> = LazyLock::new(|| {
    registry::CommandRegistry::from_sources(&[&builtin::BuiltinCommandSource])
        .expect("builtin command registry must validate")
});
```

`crates/opex-core/src/agent/commands/spec.rs`:

```rust
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandScope { Text, Native, Both }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandCategory { Session, Options, Status, Management, Media, Tools }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgType { String, Number, Boolean }

#[derive(Debug, Clone, Serialize)]
pub struct Choice { pub value: String, pub label: String }

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Choices { Static { values: Vec<Choice> }, Dynamic { provider: String } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility { All, BaseOnly }

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CommandSourceKind { Builtin, Handler { handler_id: String } }

#[derive(Debug, Clone, Serialize)]
pub struct CommandArg {
    pub name: String,
    pub description: String,
    pub arg_type: ArgType,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Choices>,
    pub capture_remaining: bool,
    pub menu: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub category: CommandCategory,
    pub scope: CommandScope,
    pub args: Vec<CommandArg>,
    pub visibility: Visibility,
    pub source: CommandSourceKind,
}

/// Результат исполнения команды (F1). `Menu` задействуется в Фазе 2 (argsMenu).
#[derive(Debug, Clone)]
pub enum CommandOutcome { Text(String), Menu { card: serde_json::Value } }

/// Санитизация в допустимое нативное имя Telegram: `[a-z0-9_]`, максимум 32.
pub fn sanitize_native_name(name: &str) -> Option<String> {
    let s: String = name.to_lowercase().chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let s = s.trim_matches('_').to_string();
    let s: String = s.chars().take(32).collect();
    if s.is_empty() { None } else { Some(s) }
}

/// Порт `assertCommandRegistry`: нет дублей имён/алиасов, консистентность scope.
pub fn validate_registry(specs: &[CommandSpec]) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    let mut aliases = std::collections::HashSet::new();
    for spec in specs {
        if !names.insert(spec.name.to_lowercase()) {
            return Err(format!("duplicate command name: {}", spec.name));
        }
        for a in &spec.aliases {
            if !aliases.insert(a.to_lowercase()) {
                return Err(format!("duplicate command alias: {a}"));
            }
        }
        if spec.scope == CommandScope::Native && !spec.aliases.is_empty() {
            return Err(format!("native-only command has text aliases: {}", spec.name));
        }
    }
    Ok(())
}
```

Добавить в `crates/opex-core/src/agent/mod.rs` строку `pub mod commands;`.

- [ ] **Step 4: Прогнать — зелёные**

Run: `cargo test -p opex-core commands::spec -- --nocapture`
Expected: PASS (4 теста).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/mod.rs crates/opex-core/src/agent/commands/spec.rs crates/opex-core/src/agent/mod.rs
git commit -m "feat(commands): CommandSpec types + registry validation + native-name sanitizer"
```

---

## Task 2: `CommandRegistry` + `CommandSource` + резолв/фильтрация

**Files:**
- Create: `crates/opex-core/src/agent/commands/registry.rs`
- Test: инлайн `#[cfg(test)]` в `registry.rs`

**Interfaces:**
- Consumes: типы Task 1.
- Produces:
  - `trait CommandSource { fn specs(&self) -> Vec<CommandSpec>; }`
  - `struct CommandRegistry { specs: Vec<CommandSpec> }`
  - `fn from_sources(sources: &[&dyn CommandSource]) -> Result<Self, String>` (валидирует)
  - `fn resolve(&self, name: &str) -> Option<&CommandSpec>` (по имени ИЛИ алиасу, ведущий `/` игнорируется)
  - `fn visible_for(&self, is_base: bool) -> Vec<&CommandSpec>` (фильтр по `Visibility`)
  - `fn all(&self) -> &[CommandSpec]`

- [ ] **Step 1: Написать падающий тест резолва и фильтрации**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::commands::spec::*;

    struct Fake(Vec<CommandSpec>);
    impl CommandSource for Fake { fn specs(&self) -> Vec<CommandSpec> { self.0.clone() } }

    fn s(name: &str, aliases: &[&str], vis: Visibility) -> CommandSpec {
        CommandSpec {
            name: name.into(), aliases: aliases.iter().map(|a| a.to_string()).collect(),
            description: "d".into(), category: CommandCategory::Status,
            scope: CommandScope::Both, args: vec![], visibility: vis,
            source: CommandSourceKind::Builtin,
        }
    }

    #[test]
    fn resolves_by_name_and_alias() {
        let src = Fake(vec![s("think", &["t"], Visibility::All)]);
        let reg = CommandRegistry::from_sources(&[&src]).unwrap();
        assert_eq!(reg.resolve("think").unwrap().name, "think");
        assert_eq!(reg.resolve("/think").unwrap().name, "think");
        assert_eq!(reg.resolve("t").unwrap().name, "think");
        assert!(reg.resolve("nope").is_none());
    }

    #[test]
    fn base_only_hidden_for_regular() {
        let src = Fake(vec![s("status", &[], Visibility::All), s("goal", &[], Visibility::BaseOnly)]);
        let reg = CommandRegistry::from_sources(&[&src]).unwrap();
        let regular: Vec<_> = reg.visible_for(false).iter().map(|c| c.name.clone()).collect();
        assert_eq!(regular, vec!["status"]);
        assert_eq!(reg.visible_for(true).len(), 2);
    }
}
```

- [ ] **Step 2: Прогнать — падает**

Run: `cargo test -p opex-core commands::registry -- --nocapture`
Expected: FAIL — `CommandRegistry` не определён.

- [ ] **Step 3: Реализовать реестр**

`crates/opex-core/src/agent/commands/registry.rs`:

```rust
use super::spec::{CommandSpec, Visibility, validate_registry};

pub trait CommandSource {
    fn specs(&self) -> Vec<CommandSpec>;
}

pub struct CommandRegistry { specs: Vec<CommandSpec> }

impl CommandRegistry {
    pub fn from_sources(sources: &[&dyn CommandSource]) -> Result<Self, String> {
        let mut specs = Vec::new();
        for src in sources { specs.extend(src.specs()); }
        validate_registry(&specs)?;
        Ok(Self { specs })
    }

    /// Резолв по каноническому имени ИЛИ алиасу; ведущий `/` игнорируется.
    pub fn resolve(&self, name: &str) -> Option<&CommandSpec> {
        let n = name.trim().trim_start_matches('/').to_lowercase();
        self.specs.iter().find(|c| {
            c.name.to_lowercase() == n || c.aliases.iter().any(|a| a.to_lowercase() == n)
        })
    }

    pub fn visible_for(&self, is_base: bool) -> Vec<&CommandSpec> {
        self.specs.iter().filter(|c| is_base || c.visibility == Visibility::All).collect()
    }

    pub fn all(&self) -> &[CommandSpec] { &self.specs }
}
```

- [ ] **Step 4: Прогнать — зелёные**

Run: `cargo test -p opex-core commands::registry -- --nocapture`
Expected: PASS (2 теста).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/registry.rs
git commit -m "feat(commands): CommandRegistry with alias resolve + visibility filter"
```

---

## Task 3: `BuiltinCommandSource` — дескрипторы 14 команд (все `Visibility::All`)

**Files:**
- Create: `crates/opex-core/src/agent/commands/builtin.rs`
- Test: инлайн `#[cfg(test)]` в `builtin.rs`

**Interfaces:**
- Consumes: типы Task 1, трейт `CommandSource` Task 2, синглтон `COMMAND_REGISTRY` Task 1.
- Produces:
  - `struct BuiltinCommandSource;` реализует `CommandSource`.
  - `pub const BUILTIN_NAMES: &[&str]` — канонический перечень (для drift-гарда Task 4).

**Правила (P2/P3):** `description` — непустые статичные **английские** строки (питают `/api/commands` + автодополнение; локализация описаний — Фаза 2 вместе с регенерацией `/help`). Все 14 команд — `Visibility::All` (base-гейтинг не вводим). `scope` — `Both` (все 14 имён проходят `sanitize_native_name`). `args`/`choices` — точно по текущему парсингу в `commands.rs` (`parse_rollback_command`, `parse_voice_command`, `parse_goal_command`, `/think`-матчинг).

- [ ] **Step 1: Написать падающий тест состава + choices**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::commands::registry::CommandRegistry;

    #[test]
    fn builtin_source_has_all_14_and_validates() {
        let reg = CommandRegistry::from_sources(&[&BuiltinCommandSource]).expect("registry valid");
        for name in BUILTIN_NAMES { assert!(reg.resolve(name).is_some(), "missing builtin: {name}"); }
        assert_eq!(reg.all().len(), BUILTIN_NAMES.len());
    }

    #[test]
    fn all_builtins_are_visible_to_everyone() {
        let reg = CommandRegistry::from_sources(&[&BuiltinCommandSource]).unwrap();
        assert_eq!(reg.visible_for(false).len(), BUILTIN_NAMES.len(), "no base-only builtins in Phase 1");
    }

    #[test]
    fn think_has_choices_and_nonempty_description() {
        let reg = CommandRegistry::from_sources(&[&BuiltinCommandSource]).unwrap();
        let think = reg.resolve("think").unwrap();
        assert!(!think.args.is_empty());
        for c in reg.all() { assert!(!c.description.is_empty(), "empty description: {}", c.name); }
    }
}
```

- [ ] **Step 2: Прогнать — падает**

Run: `cargo test -p opex-core commands::builtin -- --nocapture`
Expected: FAIL — `BuiltinCommandSource` не определён.

- [ ] **Step 3: Реализовать дескрипторы**

`crates/opex-core/src/agent/commands/builtin.rs`:

```rust
use super::registry::CommandSource;
use super::spec::*;

pub const BUILTIN_NAMES: &[&str] = &[
    "status", "new", "reset", "compact", "rollback", "model", "think",
    "voice", "usage", "export", "help", "memory", "goal", "subgoal",
];

fn simple(name: &str, desc: &str, cat: CommandCategory) -> CommandSpec {
    CommandSpec {
        name: name.into(), aliases: vec![], description: desc.into(),
        category: cat, scope: CommandScope::Both, args: vec![],
        visibility: Visibility::All, source: CommandSourceKind::Builtin,
    }
}

fn choice(v: &str) -> Choice { Choice { value: v.into(), label: v.into() } }

fn arg_choices(name: &str, desc: &str, values: &[&str], menu: bool) -> CommandArg {
    CommandArg {
        name: name.into(), description: desc.into(), arg_type: ArgType::String,
        required: false, choices: Some(Choices::Static { values: values.iter().map(|v| choice(v)).collect() }),
        capture_remaining: false, menu,
    }
}

fn arg_free(name: &str, desc: &str) -> CommandArg {
    CommandArg {
        name: name.into(), description: desc.into(), arg_type: ArgType::String,
        required: false, choices: None, capture_remaining: true, menu: false,
    }
}

pub struct BuiltinCommandSource;

impl CommandSource for BuiltinCommandSource {
    fn specs(&self) -> Vec<CommandSpec> {
        let think = {
            let mut c = simple("think", "Set thinking level", CommandCategory::Options);
            c.aliases = vec!["t".into()];
            c.args = vec![arg_choices("level", "off..max",
                &["off", "minimal", "low", "medium", "high", "max"], true)];
            c
        };
        let voice = {
            let mut c = simple("voice", "Toggle voice replies for this chat", CommandCategory::Media);
            c.args = vec![arg_choices("mode", "on|off|status", &["on", "off", "status"], true)];
            c
        };
        let model = {
            let mut c = simple("model", "Show or set the model", CommandCategory::Options);
            c.args = vec![CommandArg { name: "model".into(),
                description: "provider/model | reset | status".into(), arg_type: ArgType::String,
                required: false, choices: Some(Choices::Dynamic { provider: "models".into() }),
                capture_remaining: false, menu: false }];
            c
        };
        let rollback = {
            let mut c = simple("rollback", "Restore a checkpoint", CommandCategory::Management);
            c.args = vec![arg_free("action", "list | N | diff N | N file <path>")];
            c
        };
        let memory = {
            let mut c = simple("memory", "Search or list agent memory", CommandCategory::Status);
            c.args = vec![arg_free("query", "search query (empty = recent)")];
            c
        };
        let goal = {
            let mut c = simple("goal", "Set/inspect the autonomous goal", CommandCategory::Management);
            c.args = vec![arg_free("text", "goal | status | pause | resume | clear")];
            c
        };
        let subgoal = {
            let mut c = simple("subgoal", "Manage subgoals", CommandCategory::Management);
            c.args = vec![arg_free("action", "add <t> | list | remove <n>")];
            c
        };
        let compact = {
            let mut c = simple("compact", "Compact the session context", CommandCategory::Session);
            c.args = vec![arg_free("instructions", "extra compaction instructions")];
            c
        };
        vec![
            simple("status", "Show current status", CommandCategory::Status),
            simple("new", "Start a new session", CommandCategory::Session),
            simple("reset", "Reset the session and unpinned memory", CommandCategory::Session),
            compact,
            rollback,
            model,
            think,
            voice,
            simple("usage", "Show token usage", CommandCategory::Status),
            simple("export", "Export the current session transcript", CommandCategory::Status),
            simple("help", "Show available commands", CommandCategory::Status),
            memory,
            goal,
            subgoal,
        ]
    }
}
```

- [ ] **Step 4: Прогнать — зелёные**

Run: `cargo test -p opex-core commands::builtin -- --nocapture`
Expected: PASS (3 теста).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/agent/commands/builtin.rs
git commit -m "feat(commands): BuiltinCommandSource descriptors for 14 slash commands (all visible)"
```

---

## Task 4: `CommandOutcome`-контракт (паритет) + гард дрейфа

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/commands.rs` (тип успеха `String` → `CommandOutcome::Text`; `const DISPATCH_NAMES` + drift-тест)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs:170`
- Modify: `crates/opex-core/src/agent/pipeline/bootstrap.rs:29`
- Modify: `crates/opex-core/src/agent/engine/run.rs:172,394,574`
- Test: инлайн в `commands.rs` (drift-гард)

**Interfaces:**
- Consumes: `CommandOutcome` (Task 1), `BUILTIN_NAMES` (Task 3).
- Produces:
  - `commands::handle_command(...) -> Option<Result<CommandOutcome>>`
  - `AgentEngine::handle_command(...) -> Option<Result<CommandOutcome>>`
  - `const DISPATCH_NAMES: &[&str]` в `commands.rs` — имена, реально обрабатываемые `match` (без ведущего `/`).

**Ключевая идея:** тела `match` НЕ трогаем — меняем только тип возврата (`Some(Ok(String))` → `Some(Ok(CommandOutcome::Text(String)))`). `/help` остаётся как есть (возвращает `s.help_text`). Диспетч остаётся `match`; реестр в Фазе 1 не исполняет. **P4:** drift-гард сверяет `DISPATCH_NAMES` (стороны `match`) с `BUILTIN_NAMES` (реестр) — рассинхрон ловится тестом.

- [ ] **Step 1: Написать drift-гард тест**

В `#[cfg(test)] mod tests` в `commands.rs`:

```rust
#[test]
fn dispatch_names_match_registry_builtins() {
    use crate::agent::commands::builtin::BUILTIN_NAMES;
    let mut dispatch: Vec<&str> = super::DISPATCH_NAMES.to_vec();
    let mut builtin: Vec<&str> = BUILTIN_NAMES.to_vec();
    dispatch.sort_unstable();
    builtin.sort_unstable();
    assert_eq!(dispatch, builtin,
        "match-диспетч и BUILTIN_NAMES разъехались — обновите обе стороны");
}
```

- [ ] **Step 2: Прогнать — падает (DISPATCH_NAMES не определён)**

Run: `cargo test -p opex-core commands::pipeline commands::tests::dispatch_names -- --nocapture`
(либо `cargo test -p opex-core dispatch_names_match_registry`)
Expected: FAIL.

- [ ] **Step 3: Добавить `DISPATCH_NAMES` + сменить тип возврата**

В `commands.rs`:

1. Рядом с `handle_command` добавить:
   ```rust
   /// Имена, реально обрабатываемые `match` в `handle_command` (без ведущего `/`).
   /// Держать синхронно с ветками ниже; drift-гард-тест сверяет с BUILTIN_NAMES.
   pub const DISPATCH_NAMES: &[&str] = &[
       "status", "new", "reset", "compact", "rollback", "model", "think",
       "voice", "usage", "export", "help", "memory", "goal", "subgoal",
   ];
   ```
2. Импорт: `use crate::agent::commands::spec::CommandOutcome;`
3. Сигнатура: `-> Option<Result<String>>` → `-> Option<Result<CommandOutcome>>`.
4. Механически обернуть каждый успех: `Some(Ok(x))` где `x: String` → `Some(Ok(CommandOutcome::Text(x)))`; аналогично для `return Some(Ok(...))` внутри веток. `Some(Err(_))`, `None`, `_ => None` — не трогать.

Пример (`/status`):
```rust
Some(Ok(CommandOutcome::Text(
    localization::fmt(s.status_format, &[/* … без изменений */])
)))
```

- [ ] **Step 4: Обновить обёртку `context_builder.rs:170`**

`-> Option<Result<String>>` → `-> Option<Result<CommandOutcome>>` (+ импорт типа). Тело не меняется.

- [ ] **Step 5: Обновить `BootstrapOutcome` `bootstrap.rs:29`**

`pub command_output: Option<String>,` → `pub command_output: Option<CommandOutcome>,` (+ `use crate::agent::commands::spec::CommandOutcome;`). Строки 351-354 (`Some(result) => Some(result?)`) не меняются.

- [ ] **Step 6: Обновить три early-exit блока `run.rs` (172, 394, 574)**

В КАЖДОМ из трёх блоков `if let Some(text) = command_output.take() { … }` заменить связывание:

```rust
if let Some(outcome) = command_output.take() {
    let text = match outcome {
        CommandOutcome::Text(t) => t,
        // Фаза 1: builtin-команды не порождают Menu; безопасный фолбэк.
        // Реальную RichCard-эмиссию добавит Фаза 2.
        CommandOutcome::Menu { card } => card.to_string(),
    };
    // …остальной код блока без изменений (использует `text`)
}
```

Импорт `use crate::agent::commands::spec::CommandOutcome;` в `run.rs`.

- [ ] **Step 7: Собрать + все тесты команд**

Run: `cargo test -p opex-core commands:: -- --nocapture && cargo check -p opex-core --all-targets`
Expected: PASS (включая drift-гард и существующие парсер-тесты) + чистая компиляция.

- [ ] **Step 8: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/commands.rs crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/agent/pipeline/bootstrap.rs crates/opex-core/src/agent/engine/run.rs
git commit -m "refactor(commands): CommandOutcome contract + registry/dispatch drift guard, parity preserved"
```

---

## Task 5: `GET /api/commands` эндпоинт

**Files:**
- Create: `crates/opex-core/src/gateway/handlers/commands.rs`
- Modify: `crates/opex-core/src/gateway/handlers/mod.rs` (`mod commands;` + `.merge(commands::routes())`)
- Test: инлайн — сериализация реестра в JSON

**Interfaces:**
- Consumes: синглтон `COMMAND_REGISTRY` (Task 1).
- Produces:
  - `pub(crate) fn routes() -> Router<AppState>` c `GET /api/commands`.
  - Query: `?scope=<text|native|both>` (опционально; в Фазе 1 фильтр по scope, без per-agent — все команды `All`).
  - Ответ: `{ "commands": [CommandSpec…], "version": "<n>" }`.

> **P3/P1:** per-agent-фильтр (`agent`/`is_base`) в Фазе 1 не нужен — все команды `Visibility::All`. Параметр `agent` принимается, но игнорируется (совместимость с UI-хуком). Реестр — из синглтона, не из `AppState`.

- [ ] **Step 1: Тест сериализации**

`crates/opex-core/src/gateway/handlers/commands.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn commands_serialize_to_json_array() {
        let reg = &*crate::agent::commands::COMMAND_REGISTRY;
        let json = serde_json::to_value(reg.all()).unwrap();
        assert!(json.as_array().unwrap().len() >= 14);
        assert!(json[0].get("name").is_some());
    }
}
```

- [ ] **Step 2: Прогнать — падает (модуль/тест не найден)**

Run: `cargo test -p opex-core gateway::handlers::commands -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Реализовать handler**

`crates/opex-core/src/gateway/handlers/commands.rs`:

```rust
use axum::{extract::Query, response::IntoResponse, routing::get, Json, Router};
use serde::Deserialize;
use crate::agent::commands::spec::CommandScope;
use crate::gateway::state::AppState;

#[derive(Deserialize)]
struct CommandsQuery {
    #[allow(dead_code)] agent: Option<String>,
    #[allow(dead_code)] lang: Option<String>,
    scope: Option<String>,
}

async fn list_commands(Query(q): Query<CommandsQuery>) -> impl IntoResponse {
    let reg = &*crate::agent::commands::COMMAND_REGISTRY;
    // Фаза 1: все команды Visibility::All → visible_for(false) == все.
    let mut specs = reg.visible_for(false);
    if q.scope.as_deref() == Some("native") {
        specs.retain(|c| matches!(c.scope, CommandScope::Native | CommandScope::Both));
    }
    let version = specs.len().to_string(); // F8: простой version-tag; ETag-версия — Фаза 2
    Json(serde_json::json!({ "commands": specs, "version": version }))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/commands", get(list_commands))
}
```

В `handlers/mod.rs`: `mod commands;` + в композиции роутера `.merge(commands::routes())`. Auth: `/api/commands` попадает под общий auth-middleware `/api/*` (как остальные), отдельной настройки не требует.

- [ ] **Step 4: Прогнать — зелёный + curl-smoke после деплоя**

Run: `cargo test -p opex-core gateway::handlers::commands -- --nocapture`
Expected: PASS.
После деплоя: `curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:18789/api/commands | jq '.commands | length'` → 14.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/commands.rs crates/opex-core/src/gateway/handlers/mod.rs
git commit -m "feat(api): GET /api/commands returns the command registry"
```

---

## Task 6: Web-автодополнение `/` в композере

**Files:**
- Create: `ui/src/hooks/use-commands.ts`
- Create: `ui/src/components/chat/command-autocomplete.tsx`
- Create: `ui/src/components/chat/command-autocomplete.test.tsx`
- Modify: композер (компонент с `textarea` в `ui/src/components/chat/`)
- Modify: `ui/src/types/api.ts` (тип `CommandInfo`)

**Interfaces:**
- Consumes: `GET /api/commands`, хелпер `apiGet<T>` из `@/lib/api` (P5).
- Produces:
  - `useCommands(agent: string): { data?: CommandInfo[] }`
  - `<CommandAutocomplete input={string} commands={CommandInfo[]} onPick={(name:string)=>void} />`
  - `interface CommandInfo { name, description, category, aliases, args }` в `types/api.ts`.

> **Vitest-готча:** запускать **только из `ui/`** (не из корня репо).

- [ ] **Step 1: Написать падающий тест компонента**

`ui/src/components/chat/command-autocomplete.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { CommandAutocomplete } from "./command-autocomplete";

const cmds = [
  { name: "status", description: "Show status", category: "status", aliases: [], args: [] },
  { name: "summarize_video", description: "Summarize a video", category: "media", aliases: [], args: [{ name: "url" }] },
];

it("filters commands by prefix after slash", () => {
  render(<CommandAutocomplete input="/sum" commands={cmds} onPick={() => {}} />);
  expect(screen.getByText(/summarize_video/)).toBeInTheDocument();
  expect(screen.queryByText(/status/)).not.toBeInTheDocument();
});

it("renders nothing without leading slash", () => {
  const { container } = render(<CommandAutocomplete input="hello" commands={cmds} onPick={() => {}} />);
  expect(container).toBeEmptyDOMElement();
});
```

- [ ] **Step 2: Прогнать — падает**

Run (из `ui/`): `npm test -- command-autocomplete`
Expected: FAIL — компонент не существует.

- [ ] **Step 3: Реализовать тип + хук + компонент**

`ui/src/types/api.ts` — добавить (hand-mirror gateway-DTO, как `AgentInfo`; ts-rs не участвует):

```ts
export interface CommandArgInfo { name: string; description?: string; required?: boolean; }
export interface CommandInfo {
  name: string; description: string; category: string;
  aliases: string[]; args: CommandArgInfo[];
}
```

`ui/src/hooks/use-commands.ts` (P5 — `apiGet<T>` возвращает распарсенный `T`):

```ts
import { useQuery } from "@tanstack/react-query";
import { apiGet } from "@/lib/api";
import type { CommandInfo } from "@/types/api";

export function useCommands(agent: string) {
  return useQuery({
    queryKey: ["commands", agent],
    queryFn: async (): Promise<CommandInfo[]> => {
      const j = await apiGet<{ commands: CommandInfo[] }>(`/api/commands?agent=${encodeURIComponent(agent)}`);
      return j.commands ?? [];
    },
    staleTime: 60_000,
  });
}
```

`ui/src/components/chat/command-autocomplete.tsx`:

```tsx
import type { CommandInfo } from "@/types/api";

export function CommandAutocomplete({
  input, commands, onPick,
}: { input: string; commands: CommandInfo[]; onPick: (name: string) => void }) {
  if (!input.startsWith("/")) return null;
  const q = input.slice(1).toLowerCase();
  const matches = commands.filter(
    (c) => c.name.toLowerCase().startsWith(q) || c.aliases.some((a) => a.toLowerCase().startsWith(q)),
  );
  if (matches.length === 0) return null;
  return (
    <div className="absolute bottom-full mb-1 w-full max-h-64 overflow-y-auto rounded-md border bg-popover shadow-md">
      {matches.map((c) => (
        <button key={c.name} type="button"
          className="flex w-full items-baseline gap-2 px-3 py-1.5 text-left hover:bg-accent"
          onClick={() => onPick(c.name)}>
          <span className="font-mono text-sm">/{c.name}</span>
          {c.args.length > 0 && (
            <span className="font-mono text-xs text-muted-foreground">
              {c.args.map((a) => `<${a.name}>`).join(" ")}
            </span>
          )}
          <span className="ml-auto truncate text-xs text-muted-foreground">{c.description}</span>
        </button>
      ))}
    </div>
  );
}
```

- [ ] **Step 4: Прогнать — зелёный**

Run (из `ui/`): `npm test -- command-autocomplete`
Expected: PASS (2 теста).

- [ ] **Step 5: Интегрировать в композер**

В компоненте композера: `const { data } = useCommands(agent);`, обернуть `textarea` в `relative`-контейнер, рендерить `<CommandAutocomplete input={draft} commands={data ?? []} onPick={(n) => setDraft(`/${n} `)} />`. Клавиатурная навигация — опционально (клик достаточно для Фазы 1).

- [ ] **Step 6: Билд UI**

Run (из `ui/`): `npm run build`
Expected: сборка без TS-ошибок.

- [ ] **Step 7: Commit**

```bash
git add ui/src/hooks/use-commands.ts ui/src/components/chat/command-autocomplete.tsx ui/src/components/chat/command-autocomplete.test.tsx ui/src/types/api.ts
git commit -m "feat(ui): slash-command autocomplete in chat composer"
```

---

## Task 7: Интеграция, деплой, E2E-паритет

**Files:** нет новых — сборка/деплой/проверка.

- [ ] **Step 1: Воркспейс-тесты + gen-types drift (P7)**

Run (сервер): `cargo test --workspace` и `make gen-types` (проверить, что рабочее дерево чисто — `CommandSpec` не деривит `TS`, drift быть не должно).
Expected: тесты зелёные (DB-тесты без PG падают ожидаемо — гонять `make test-db` при наличии PG); `git status` после `gen-types` — без изменений.

- [ ] **Step 2: Деплой**

Run: `make remote-deploy` (Rust) + `scripts/deploy-ui.sh` (UI, абсолютным путём).
Expected: атомарный своп + рестарт; `make doctor` зелёный.

- [ ] **Step 3: E2E-паритет 14 команд (Telegram + web)**

Прогнать каждую: `/status`, `/new`, `/reset`, `/compact`, `/rollback`, `/model`, `/think medium`, `/voice status`, `/usage`, `/export`, `/help`, `/memory`, `/goal`, `/subgoal list`.
Expected: вывод **идентичен** до-рефактору (включая локализованный `/help` — он не менялся). Это же покрывает drift-гард на живых данных.

- [ ] **Step 4: E2E автодополнения**

В web-чате ввести `/` → список; `/st` → фильтр до `/status`; клик подставляет `/status `.
Expected: работает.

- [ ] **Step 5: `/api/commands` smoke**

Run: `curl -s -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:18789/api/commands" | jq '.commands|length'`
Expected: 14. `?scope=native` — тоже 14 (все `Both`).

- [ ] **Step 6: Маркер завершения Фазы 1**

```bash
git commit --allow-empty -m "chore(commands): Phase 1 complete — registry + builtins + /api/commands + web autocomplete"
```

---

## Фаза 2 и Фаза 3 (task-level outline — детальный под-план перед исполнением)

Интерфейсы Фазы 1 (`CommandSpec`, `CommandRegistry`, `CommandSource`, `CommandOutcome`, `/api/commands`) затвердевают в Фазе 1; детальные bite-sized под-планы Фаз 2–3 пишутся отдельно.

**Фаза 2 — handlers-as-commands + меню + локализованный `/help`:**
1. **Проводка динамического реестра.** `LazyLock`-синглтон Фазы 1 расширяется до реестра, знающего о `HandlerRegistry`. Диспетч handler-команд в bootstrap достаёт `toolgate_url`/`http_client`/`db` тем же путём, что тул `file_handler` (через `ToolDeps`-подобные зависимости) — движок сам их не держит.
2. `HandlerCommandSource` — деривация из манифестов (имя=id, arg `source`, валвсы→арги, `<command>`-оверрайд). Приоритет builtin; конфликтные алиасы отбрасываются.
3. Резолв источника: арг → `msg.attachments` → джойн `uploads`↔`messages` (F4) → argsMenu.
4. Диспетч handler-команды → `insert_handler_job` + трастовый гейт `match_buttons`/`match_url_handlers` (F6).
5. `CommandOutcome::Menu` эмиссия через `StreamEvent::RichCard` в трёх блоках `run.rs`; card-type `command_args_menu` в [sink.rs:144](../../crates/opex-core/src/agent/pipeline/sink.rs) + web `card-registry.tsx` (F1, F7).
6. **Регенерация `/help` + новый `/commands` из реестра, локализованные** (перенос описаний builtin в `localization`; заголовки категорий из `get_strings(lang)`) — то, что отложено из Фазы 1 (P2).
7. Обобщённый run-эндпоинт (`/api/files/menu-run` → команды) + Telegram inline-callback `(command, arg, value)`.
8. Telegram `setMyCommands` из `GET /api/commands?scope=native` после handshake; удаление статического списка [telegram.ts:200](../../channels/src/drivers/telegram.ts); выпил channel-side `cmd*`-строк (F2, F5).
9. Per-agent видимость + возможный base-гейтинг команд (enforce в диспетче через реестр) — если решим вводить (P3 отложено).
10. Версионирование `/api/commands` по ETag `HandlerRegistry` (F8).

**Фаза 3 — Discord:**
1. Регистрация application-commands при старте адаптера (F10 — greenfield).
2. `interactionCreate` → ack/deferReply → трансляция в inbound `/cmd args` → тот же диспетчер.
3. Типизированные options + choices из `CommandSpec.args`.

---

## Self-Review (Фаза 1)

- **Покрытие спека (Фаза 1):** типы+валидация (Task 1 ✓), реестр+резолв (Task 2 ✓), builtins все-`All` (Task 3 ✓), `CommandOutcome`-контракт F1 + drift-гард P4 (Task 4 ✓), `/api/commands` + scope-фильтр + version F8-базово (Task 5 ✓), web-автодополнение P5 (Task 6 ✓), native-name-санитайзер F3 (Task 1 ✓), паритет E2E + gen-types P7 (Task 7 ✓). Отложено в Фазу 2: регенерация/локализация `/help` (P2), handler-команды, меню (F1-эмиссия), Telegram/Discord (F2/F5/F10), per-agent/base-гейтинг (P3).
- **Плейсхолдеры:** все 14 дескрипторов в Task 3 заданы полностью (с choices / capture_remaining / простые). Описания — непустые английские (тест это проверяет).
- **Согласованность типов:** `CommandOutcome`, `CommandSpec`, `visible_for`, `resolve`, `from_sources`, `sanitize_native_name`, `COMMAND_REGISTRY`, `BUILTIN_NAMES`, `DISPATCH_NAMES`, `CommandInfo`/`apiGet` — имена совпадают между задачами.
- **Ревью-фиксы:** P1 (LazyLock, без правок `AppState`/`EngineConfig`/`CommandContext`) ✓, P2 (`/help` не меняется в Фазе 1) ✓, P3 (все `All`, тест `all_builtins_are_visible_to_everyone`) ✓, P4 (drift-гард) ✓, P5 (`apiGet`) ✓, P6 (`State<AppState>` не нужен — синглтон) ✓, P7 (gen-types проверка в Task 7) ✓.
