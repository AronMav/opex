use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandScope {
    // consumed in Phase 2: text-only commands with no native-command mirror
    #[allow(dead_code)]
    Text,
    Native,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandCategory {
    Session,
    Options,
    Status,
    Management,
    Media,
    // consumed in Phase 2: category for handler-sourced tool commands
    #[allow(dead_code)]
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    String,
    // consumed in Phase 2: typed args for handler-sourced commands
    #[allow(dead_code)]
    Number,
    #[allow(dead_code)]
    Boolean,
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice { pub value: String, pub label: String }

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Choices { Static { values: Vec<Choice> }, Dynamic { provider: String } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    All,
    // consumed in Phase 2: base-agent-only command gating (not introduced in Phase 1)
    #[allow(dead_code)]
    BaseOnly,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CommandSourceKind {
    Builtin,
    // consumed in Phase 2: commands sourced from toolgate file handlers
    #[allow(dead_code)]
    Handler { handler_id: String },
}

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
pub enum CommandOutcome {
    Text(String),
    // consumed in Phase 2: argsMenu-driven interactive command results
    #[allow(dead_code)]
    Menu { card: serde_json::Value },
}

/// Санитизация в допустимое нативное имя Telegram: `[a-z0-9_]`, максимум 32.
// consumed in Phase 2: native-command registration (Telegram BotFather sync)
#[allow(dead_code)]
pub fn sanitize_native_name(name: &str) -> Option<String> {
    let s: String = name.to_lowercase().chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let s = s.trim_matches('_').to_string();
    let s: String = s.chars().take(32).collect();
    if s.is_empty() { None } else { Some(s) }
}

/// Порт `assertCommandRegistry`: нет дублей имён/алиасов, консистентность scope.
///
/// Names and aliases share one namespace — a typed `/token` resolves against
/// either, so a name that collides with a different spec's alias is ambiguous
/// and must be rejected. Both are seeded into a single `taken` set (names
/// first, so a name↔alias clash surfaces regardless of spec order); this is the
/// airtightness `merge.rs`'s `.expect("merged registry must validate")` relies on.
pub fn validate_registry(specs: &[CommandSpec]) -> Result<(), String> {
    let mut taken = std::collections::HashSet::new();
    for spec in specs {
        if !taken.insert(spec.name.to_lowercase()) {
            return Err(format!("duplicate command name: {}", spec.name));
        }
    }
    for spec in specs {
        for a in &spec.aliases {
            if !taken.insert(a.to_lowercase()) {
                return Err(format!("alias collides with an existing name or alias: {a}"));
            }
        }
        if spec.scope == CommandScope::Native && !spec.aliases.is_empty() {
            return Err(format!("native-only command has text aliases: {}", spec.name));
        }
    }
    Ok(())
}

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
    fn name_colliding_with_other_specs_alias_rejected() {
        // `/t` would be ambiguous: it's `think`'s alias AND a command name.
        let mut think = spec("think", CommandScope::Both); think.aliases = vec!["t".into()];
        let t = spec("t", CommandScope::Both);
        assert!(validate_registry(&[think, t]).is_err());
        // Order-independent: name before or after the aliasing spec both reject.
        let mut think2 = spec("think", CommandScope::Both); think2.aliases = vec!["t".into()];
        let t2 = spec("t", CommandScope::Both);
        assert!(validate_registry(&[t2, think2]).is_err());
    }

    #[test]
    fn native_name_sanitized() {
        assert_eq!(sanitize_native_name("export-session").as_deref(), Some("export_session"));
        assert_eq!(sanitize_native_name("Summarize_Video").as_deref(), Some("summarize_video"));
        assert_eq!(sanitize_native_name("---").as_deref(), None);
    }
}
