//! Command registry: aggregates specs from multiple sources, resolves by
//! name/alias, and filters by visibility.

use super::spec::{CommandSpec, Visibility, validate_registry};

pub trait CommandSource {
    fn specs(&self) -> Vec<CommandSpec>;
}

pub struct CommandRegistry {
    specs: Vec<CommandSpec>,
}

impl CommandRegistry {
    pub fn from_sources(sources: &[&dyn CommandSource]) -> Result<Self, String> {
        let mut specs = Vec::new();
        for src in sources {
            specs.extend(src.specs());
        }
        validate_registry(&specs)?;
        Ok(Self { specs })
    }

    /// Резолв по каноническому имени ИЛИ алиасу; ведущий `/` игнорируется.
    // consumed in Phase 2: slash-dispatch will resolve through the registry
    // directly (Phase 1 dispatch still runs through the legacy match table in
    // `agent::pipeline::commands`); exercised by unit tests today.
    #[allow(dead_code)]
    pub fn resolve(&self, name: &str) -> Option<&CommandSpec> {
        let n = name.trim().trim_start_matches('/').to_lowercase();
        self.specs.iter().find(|c| {
            c.name.to_lowercase() == n || c.aliases.iter().any(|a| a.to_lowercase() == n)
        })
    }

    pub fn visible_for(&self, is_base: bool) -> Vec<&CommandSpec> {
        self.specs
            .iter()
            .filter(|c| is_base || c.visibility == Visibility::All)
            .collect()
    }

    // consumed in Phase 2: bulk registry export beyond `/api/commands`
    // (which filters via `visible_for`); exercised by unit tests today.
    #[allow(dead_code)]
    pub fn all(&self) -> &[CommandSpec] {
        &self.specs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::commands::spec::*;

    struct Fake(Vec<CommandSpec>);
    impl CommandSource for Fake {
        fn specs(&self) -> Vec<CommandSpec> {
            self.0.clone()
        }
    }

    fn s(name: &str, aliases: &[&str], vis: Visibility) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            description: "d".into(),
            category: CommandCategory::Status,
            scope: CommandScope::Both,
            args: vec![],
            visibility: vis,
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
        let src = Fake(vec![
            s("status", &[], Visibility::All),
            s("goal", &[], Visibility::BaseOnly),
        ]);
        let reg = CommandRegistry::from_sources(&[&src]).unwrap();
        let regular: Vec<_> = reg.visible_for(false).iter().map(|c| c.name.clone()).collect();
        assert_eq!(regular, vec!["status"]);
        assert_eq!(reg.visible_for(true).len(), 2);
    }
}
