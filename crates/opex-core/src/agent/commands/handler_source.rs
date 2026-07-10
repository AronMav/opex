//! Derives `CommandSpec`s from live `HandlerRegistry` manifests (Phase 2a).
//!
//! Only `execution == "async"` handlers become chat commands (sync handlers
//! run inline via the existing file-handler menu and have no async-command
//! shape to poll). Builtin-tier handlers are further gated by the operator's
//! `fse.allowlist` (`enabled`) — same allowlist the composer menu uses.

use super::registry::CommandSource;
use super::spec::*;
use crate::agent::handler_registry::HandlerManifest;

fn desc_for<'a>(m: &'a HandlerManifest, lang: &str) -> String {
    m.descriptions
        .get(lang)
        .or_else(|| m.descriptions.get("en"))
        .cloned()
        .unwrap_or_else(|| m.id.clone())
}

/// Optional named args from valve (`config`) fields that declare enum choices.
fn valve_args(config: &serde_json::Value, lang: &str) -> Vec<CommandArg> {
    let Some(arr) = config.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|f| {
            let name = f.get("name")?.as_str()?.to_string();
            let choices = f
                .get("choices")
                .or_else(|| f.get("enum"))
                .and_then(|c| c.as_array())
                .map(|vs| Choices::Static {
                    values: vs
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| Choice { value: s.into(), label: s.into() }))
                        .collect(),
                });
            let description = f
                .get("label")
                .and_then(|l| l.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let _ = lang; // labels are single-locale in v1
            Some(CommandArg {
                name,
                description,
                arg_type: ArgType::String,
                required: false,
                choices,
                capture_remaining: false,
                menu: true,
            })
        })
        .collect()
}

pub fn derive_handler_commands(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> Vec<CommandSpec> {
    manifests
        .iter()
        .filter(|m| m.execution == "async")
        .filter(|m| m.tier != "builtin" || enabled.iter().any(|e| e == &m.id))
        .map(|m| {
            let mut args = vec![CommandArg {
                name: "source".into(),
                description: "url or file".into(),
                arg_type: ArgType::String,
                required: false,
                choices: None,
                capture_remaining: true,
                menu: false,
            }];
            args.extend(valve_args(&m.config, lang));
            CommandSpec {
                name: m.id.clone(),
                aliases: vec![],
                description: desc_for(m, lang),
                category: CommandCategory::Media,
                scope: CommandScope::Both,
                args,
                visibility: Visibility::All,
                source: CommandSourceKind::Handler { handler_id: m.id.clone() },
            }
        })
        .collect()
}

pub struct HandlerCommandSource {
    specs: Vec<CommandSpec>,
}

impl HandlerCommandSource {
    pub fn new(manifests: &[HandlerManifest], enabled: &[String], lang: &str) -> Self {
        Self { specs: derive_handler_commands(manifests, enabled, lang) }
    }
}

impl CommandSource for HandlerCommandSource {
    fn specs(&self) -> Vec<CommandSpec> {
        self.specs.clone()
    }
}

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
        }))
        .unwrap()
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
