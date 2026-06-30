//! Core-side discovery cache + matcher for toolgate-hosted file handlers.
//! `HandlerManifest` mirrors the toolgate `GET /handlers` item wire shape;
//! `match_buttons` is the pure tiered trust gate (builtin∩allowlist,
//! workspace default-on) that turns a mime+size into composer buttons.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::agent::fse::allowlist::FSE_DEFAULT_ALLOWLIST;

/// Inner `"match"` object of a manifest: mime globs + an optional size cap.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HandlerMatch {
    #[serde(default)]
    pub mime: Vec<String>,
    #[serde(default)]
    pub max_size_mb: Option<u64>,
}

/// One handler manifest as served by toolgate `GET /handlers`. Serde field
/// names match the toolgate JSON; the nested object is read via
/// `#[serde(rename = "match")]` (Rust keyword → `match_`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandlerManifest {
    pub id: String,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub descriptions: HashMap<String, String>,
    #[serde(default)]
    pub icon: String,
    #[serde(rename = "match", default)]
    pub match_: HandlerMatch,
    #[serde(default)]
    pub capability: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub execution: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub tier: String,
}

/// A composer button derived from a manifest for a concrete file.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HandlerButton {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub params: serde_json::Value,
}

/// True if `mime` matches a glob like `audio/*` or an exact `application/pdf`.
fn mime_glob_matches(pattern: &str, mime: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        mime.split('/').next() == Some(prefix)
    } else if pattern == "*" || pattern == "*/*" {
        true
    } else {
        pattern.eq_ignore_ascii_case(mime)
    }
}

/// Localize a manifest label: requested `lang`, else `en`, else the id.
fn localize(m: &HandlerManifest, lang: &str) -> String {
    m.labels
        .get(lang)
        .or_else(|| m.labels.get("en"))
        .cloned()
        .unwrap_or_else(|| m.id.clone())
}

/// Pure tiered match: filter manifests by mime-glob + `max_size_mb`, apply the
/// trust gate by tier (builtin → must be one of the 5 const ids AND an enabled
/// member of the allowlist; workspace → allowed by default), then localize +
/// sort by `order` then id.
pub fn match_buttons(
    manifests: &[HandlerManifest],
    mime: &str,
    size: u64,
    enabled_allowlist: &[String],
    lang: &str,
) -> Vec<HandlerButton> {
    let mut matched: Vec<&HandlerManifest> = manifests
        .iter()
        .filter(|m| m.match_.mime.iter().any(|p| mime_glob_matches(p, mime)))
        .filter(|m| match m.match_.max_size_mb {
            Some(cap) => size <= cap.saturating_mul(1024 * 1024),
            None => true,
        })
        .filter(|m| match m.tier.as_str() {
            "builtin" => {
                // builtin ids hard-anchored to the const; allowlist toggle gates which are on.
                FSE_DEFAULT_ALLOWLIST.contains(&m.id.as_str())
                    && enabled_allowlist.iter().any(|x| x == &m.id)
            }
            // workspace (and any future tier) → default-on for v1 trusted authors
            _ => true,
        })
        .collect();

    matched.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    matched
        .into_iter()
        .map(|m| HandlerButton {
            id: m.id.clone(),
            label: localize(m, lang),
            icon: m.icon.clone(),
            params: m.params.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn mf(id: &str, tier: &str, mimes: &[&str], max_mb: Option<u64>, order: i32) -> HandlerManifest {
        let mut labels = HashMap::new();
        labels.insert("ru".to_string(), format!("{id}-ru"));
        labels.insert("en".to_string(), format!("{id}-en"));
        HandlerManifest {
            id: id.to_string(),
            labels,
            descriptions: HashMap::new(),
            icon: "mic".to_string(),
            match_: HandlerMatch {
                mime: mimes.iter().map(|s| s.to_string()).collect(),
                max_size_mb: max_mb,
            },
            capability: None,
            provider: None,
            execution: "sync".to_string(),
            output: "text".to_string(),
            params: serde_json::json!([]),
            order,
            tier: tier.to_string(),
        }
    }

    fn full() -> Vec<String> {
        FSE_DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manifest_deserializes_from_toolgate_json() {
        let raw = serde_json::json!({
            "id": "transcribe",
            "labels": {"ru": "Транскрибировать", "en": "Transcribe"},
            "descriptions": {"ru": "речь в текст"},
            "icon": "mic",
            "match": {"mime": ["audio/*", "video/*"], "max_size_mb": 200},
            "capability": "stt",
            "provider": "speaches-local",
            "execution": "sync",
            "output": "text",
            "params": [{"name": "language", "type": "string", "default": "ru", "required": false}],
            "order": 10,
            "tier": "builtin"
        });
        let m: HandlerManifest = serde_json::from_value(raw).unwrap();
        assert_eq!(m.id, "transcribe");
        assert_eq!(m.match_.mime, vec!["audio/*".to_string(), "video/*".to_string()]);
        assert_eq!(m.match_.max_size_mb, Some(200));
        assert_eq!(m.tier, "builtin");
        assert_eq!(m.provider.as_deref(), Some("speaches-local"));
        assert_eq!(m.labels.get("ru").map(|s| s.as_str()), Some("Транскрибировать"));
    }

    #[test]
    fn manifest_defaults_missing_optional_fields() {
        // A minimal manifest (only id + execution) must deserialize with defaults.
        let raw = serde_json::json!({"id": "save", "execution": "sync"});
        let m: HandlerManifest = serde_json::from_value(raw).unwrap();
        assert_eq!(m.id, "save");
        assert!(m.match_.mime.is_empty());
        assert!(m.match_.max_size_mb.is_none());
        assert!(m.labels.is_empty());
        assert_eq!(m.order, 0);
    }

    #[test]
    fn builtin_button_requires_allowlist_membership() {
        // builtin "transcribe" matches audio/* and is in the full allowlist → button
        let ms = vec![mf("transcribe", "builtin", &["audio/*"], Some(200), 10)];
        let out = match_buttons(&ms, "audio/ogg", 1_000, &full(), "ru");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "transcribe");
        assert_eq!(out[0].label, "transcribe-ru");

        // same builtin id, but operator disabled it in the toggle → hidden
        let enabled = vec!["describe".to_string()];
        let out2 = match_buttons(&ms, "audio/ogg", 1_000, &enabled, "ru");
        assert!(out2.is_empty(), "disabled builtin must not produce a button");
    }

    #[test]
    fn unknown_builtin_id_is_never_offered() {
        // tier=builtin but id not in the const FSE_DEFAULT_ALLOWLIST → never a button,
        // even if a hand-edited allowlist row somehow lists it.
        let ms = vec![mf("rm_rf", "builtin", &["audio/*"], None, 1)];
        let bogus_enabled = vec!["rm_rf".to_string()];
        assert!(match_buttons(&ms, "audio/ogg", 1, &bogus_enabled, "ru").is_empty());
    }

    #[test]
    fn workspace_button_is_default_on_ignoring_allowlist() {
        // a workspace-tier handler not in the allowlist still produces a button
        let ms = vec![mf("my_ocr", "workspace", &["image/*"], None, 5)];
        let out = match_buttons(&ms, "image/png", 1_000, &full(), "ru");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "my_ocr");
    }

    #[test]
    fn non_matching_mime_and_oversize_are_excluded() {
        let ms = vec![mf("transcribe", "builtin", &["audio/*"], Some(1), 10)];
        // wrong mime
        assert!(match_buttons(&ms, "image/png", 1_000, &full(), "ru").is_empty());
        // 2 MB > 1 MB cap
        assert!(match_buttons(&ms, "audio/ogg", 2 * 1024 * 1024, &full(), "ru").is_empty());
        // within cap
        assert_eq!(match_buttons(&ms, "audio/ogg", 100, &full(), "ru").len(), 1);
    }

    #[test]
    fn buttons_sorted_by_order_and_label_falls_back_to_en() {
        let ms = vec![
            mf("describe", "workspace", &["image/*"], None, 20),
            mf("save", "workspace", &["image/*"], None, 10),
        ];
        // "fr" missing → falls back to "en"
        let out = match_buttons(&ms, "image/png", 1, &full(), "fr");
        assert_eq!(out.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), vec!["save", "describe"]);
        assert_eq!(out[0].label, "save-en");
    }
}
