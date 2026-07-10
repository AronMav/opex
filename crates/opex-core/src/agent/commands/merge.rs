//! Merges `BuiltinCommandSource` + handler-derived commands into one
//! `CommandRegistry`, enforcing that a handler command can never shadow a
//! builtin (name or alias collision, case-insensitive → dropped + warn).

use super::builtin::{BuiltinCommandSource, builtin_description};
use super::handler_source::derive_handler_commands;
use super::registry::{CommandRegistry, CommandSource};
use super::spec::CommandSpec;
use crate::agent::handler_registry::HandlerManifest;
use std::collections::HashSet;

pub fn build_registry(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> CommandRegistry {
    let mut builtins = BuiltinCommandSource.specs();
    for spec in &mut builtins {
        if let Some(desc) = builtin_description(&spec.name, lang) {
            spec.description = desc.to_string();
        }
    }
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

    #[test]
    fn builtin_descriptions_are_localized_by_lang() {
        let reg_ru = build_registry(&[], &[], "ru");
        assert_eq!(reg_ru.resolve("status").unwrap().description, "Показать текущий статус");

        let reg_en = build_registry(&[], &[], "en");
        assert_eq!(reg_en.resolve("status").unwrap().description, "Show current status");
    }

    #[test]
    fn self_referential_override_alias_does_not_panic() {
        // Regression guard: a <command> whose alias repeats its own name once
        // corrupted build_registry into a validate panic (whole /api/commands
        // 500). derive_handler_commands now sanitizes aliases, so this builds.
        let m: HandlerManifest = serde_json::from_value(json!({
            "id":"summarize_video","execution":"async","tier":"workspace",
            "descriptions":{"en":"d"},"config":[],
            "command":{"name":"sv","aliases":["sv","dup","dup"]}
        }))
        .unwrap();
        let reg = build_registry(&[m], &[], "en");
        assert!(reg.resolve("sv").is_some());
        assert!(reg.resolve("dup").is_some());
        assert!(reg.resolve("status").is_some());
    }

    #[test]
    fn builtin_description_fallback() {
        use super::super::builtin::builtin_description;
        // Known builtin, unsupported/unknown lang → EN fallback.
        assert_eq!(builtin_description("status", "fr"), Some("Show current status"));
        // Not a builtin name → None.
        assert_eq!(builtin_description("not_a_builtin", "ru"), None);
    }
}
