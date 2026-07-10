//! `BuiltinCommandSource` — дескрипторы 14 существующих слэш-команд.
//!
//! Единственный источник правды для `/api/commands` + автодополнения на
//! стороне UI/каналов. Все 14 команд `Visibility::All` (base-гейтинг не
//! вводим в Фазе 1); `description` — статичные английские строки (RU
//! локализация — Фаза 2 вместе с регенерацией `/help`).

use super::registry::CommandSource;
use super::spec::*;

// consumed in Phase 2 (also exercised today by the registry/dispatch drift-guard
// test in `agent::pipeline::commands` and the builtin-coverage tests below).
#[allow(dead_code)]
pub const BUILTIN_NAMES: &[&str] = &[
    "status", "new", "reset", "compact", "rollback", "model", "think",
    "voice", "usage", "export", "help", "memory", "goal", "subgoal",
];

/// Builds a builtin spec, sourcing its English description from the single
/// `builtin_description` table (not a second hand-written copy). `build_registry`
/// overwrites `description` per-lang anyway, so this only feeds the direct
/// `specs()`/test paths — keeping it table-driven removes the drift trap where
/// the inline EN string and `builtin_description("en")` could silently diverge.
fn simple(name: &str, cat: CommandCategory) -> CommandSpec {
    CommandSpec {
        name: name.into(), aliases: vec![],
        description: builtin_description(name, "en").unwrap_or("").into(),
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

/// Localized description for a builtin command by name + language.
///
/// `"ru"` returns the Russian translation; any other lang (including
/// `"en"`) falls back to the existing English string. Returns `None` if
/// `name` is not a known builtin.
pub fn builtin_description(name: &str, lang: &str) -> Option<&'static str> {
    let en = match name {
        "status" => "Show current status",
        "new" => "Start a new session",
        "reset" => "Reset the session and unpinned memory",
        "compact" => "Compact the session context",
        "rollback" => "Restore a checkpoint",
        "model" => "Show or set the model",
        "think" => "Set thinking level",
        "voice" => "Toggle voice replies for this chat",
        "usage" => "Show token usage",
        "export" => "Export the current session transcript",
        "help" => "Show available commands",
        "memory" => "Search or list agent memory",
        "goal" => "Set/inspect the autonomous goal",
        "subgoal" => "Manage subgoals",
        _ => return None,
    };
    if lang == "ru" {
        let ru = match name {
            "status" => "Показать текущий статус",
            "new" => "Начать новую сессию",
            "reset" => "Сбросить сессию и незакреплённую память",
            "compact" => "Сжать контекст сессии",
            "rollback" => "Восстановить чекпойнт",
            "model" => "Показать или сменить модель",
            "think" => "Задать уровень размышления",
            "voice" => "Голосовые ответы для этого чата",
            "usage" => "Показать расход токенов",
            "export" => "Экспорт транскрипта сессии",
            "help" => "Показать доступные команды",
            "memory" => "Поиск и список памяти агента",
            "goal" => "Задать или посмотреть автономную цель",
            "subgoal" => "Управление подцелями",
            _ => unreachable!("name already validated against en match above"),
        };
        return Some(ru);
    }
    Some(en)
}

pub struct BuiltinCommandSource;

impl CommandSource for BuiltinCommandSource {
    fn specs(&self) -> Vec<CommandSpec> {
        let think = {
            let mut c = simple("think", CommandCategory::Options);
            c.aliases = vec!["t".into()];
            c.args = vec![arg_choices("level", "off..max",
                &["off", "minimal", "low", "medium", "high", "max"], true)];
            c
        };
        let voice = {
            let mut c = simple("voice", CommandCategory::Media);
            c.args = vec![arg_choices("mode", "on|off|status", &["on", "off", "status"], true)];
            c
        };
        let model = {
            let mut c = simple("model", CommandCategory::Options);
            c.args = vec![CommandArg { name: "model".into(),
                description: "provider/model | reset | status".into(), arg_type: ArgType::String,
                required: false, choices: Some(Choices::Dynamic { provider: "models".into() }),
                capture_remaining: false, menu: false }];
            c
        };
        let rollback = {
            let mut c = simple("rollback", CommandCategory::Management);
            c.args = vec![arg_free("action", "list | N | diff N | N file <path>")];
            c
        };
        let memory = {
            let mut c = simple("memory", CommandCategory::Status);
            c.args = vec![arg_free("query", "search query (empty = recent)")];
            c
        };
        let goal = {
            let mut c = simple("goal", CommandCategory::Management);
            c.args = vec![arg_free("text", "goal | status | pause | resume | clear")];
            c
        };
        let subgoal = {
            let mut c = simple("subgoal", CommandCategory::Management);
            c.args = vec![arg_free("action", "add <t> | list | remove <n>")];
            c
        };
        let compact = {
            let mut c = simple("compact", CommandCategory::Session);
            c.args = vec![arg_free("instructions", "extra compaction instructions")];
            c
        };
        vec![
            simple("status", CommandCategory::Status),
            simple("new", CommandCategory::Session),
            simple("reset", CommandCategory::Session),
            compact,
            rollback,
            model,
            think,
            voice,
            simple("usage", CommandCategory::Status),
            simple("export", CommandCategory::Status),
            simple("help", CommandCategory::Status),
            memory,
            goal,
            subgoal,
        ]
    }
}

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

    /// Guards the drift trap: `simple()` now sources its EN string from
    /// `builtin_description`, so every builtin name MUST resolve there for both
    /// languages — otherwise a command would ship with an empty description.
    #[test]
    fn every_builtin_has_localized_description() {
        for name in BUILTIN_NAMES {
            let en = builtin_description(name, "en").unwrap_or_else(|| panic!("no en desc: {name}"));
            let ru = builtin_description(name, "ru").unwrap_or_else(|| panic!("no ru desc: {name}"));
            assert!(!en.is_empty(), "empty en desc: {name}");
            assert!(!ru.is_empty(), "empty ru desc: {name}");
        }
        // A non-builtin name yields None (not an empty string).
        assert!(builtin_description("definitely_not_a_command", "en").is_none());
    }
}
