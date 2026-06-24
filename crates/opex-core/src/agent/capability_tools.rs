//! Built-in capability tools: один инструмент на активную media-capability.
//! Спецификации = бывшие workspace/tools/*.yaml, перенесённые в код.
//! Описание дополняется именем топ-приоритетного активного провайдера.

use crate::tools::yaml_tools::YamlToolDef;

pub struct CapabilitySpec {
    pub capability: &'static str,
    pub tool_name: &'static str,
    pub yaml: &'static str,
}

pub const CAPABILITY_TOOL_NAMES: [&str; 5] = [
    "generate_image",
    "synthesize_speech",
    "search_web",
    "transcribe_audio",
    "analyze_image",
];

pub fn is_capability_tool(name: &str) -> bool {
    CAPABILITY_TOOL_NAMES.contains(&name)
}

const GENERATE_IMAGE: &str = r#"
name: generate_image
description: "Generate images from a text description. Use for illustrations, diagrams, and art. The prompt must be in English. NOTE: the image is displayed in chat automatically — do NOT use canvas or other tools to show it."
endpoint: "http://localhost:9011/generate-image"
method: POST
parameters:
  prompt: { type: string, required: true, location: body, description: "Image description in English" }
  size: { type: string, required: false, location: body, description: "Size: 1024x1024, 1792x1024, 1024x1792, 512x512", default: "1024x1024" }
  quality: { type: string, required: false, location: body, description: "standard (fast) or high (slower, better)", default: "standard" }
channel_action: { action: send_photo, data_field: "_binary" }
status: verified
"#;

const SYNTHESIZE_SPEECH: &str = r#"
name: synthesize_speech
description: "Send a voice message to the user via the channel. Use when the user asks to read aloud, send voice, or respond by voice. The voice/timbre is determined by the agent's TTS-provider configuration. IMPORTANT: this tool dispatches the voice in the background and the audio itself IS your reply. After calling it, end your turn — do NOT write acknowledgement text."
endpoint: "http://localhost:9011/v1/audio/speech"
method: POST
timeout: 600
parameters:
  text: { type: string, required: true, location: body, description: "Text to synthesize" }
body_template: |
  {"input": "{{text}}", "response_format": "opus"}
channel_action: { action: send_voice, data_field: "_binary" }
status: verified
"#;

const SEARCH_WEB: &str = r#"
name: search_web
description: "Web search. Returns results with page-content snippets."
endpoint: "http://localhost:9011/v1/search"
method: POST
parallel: true
parameters:
  query: { type: string, required: true, location: body, description: "Search query" }
  max_results: { type: integer, required: false, location: body, description: "Maximum number of results (default 5)", default: 5 }
body_template: |
  {"query": "{{query}}"{{#if max_results}}, "max_results": {{max_results}}{{/if}}}
response_transform: "$.results"
status: verified
"#;

const TRANSCRIBE_AUDIO: &str = r#"
name: transcribe_audio
description: "Transcribe audio or a voice message from a URL. Accepts audio_url and optional language. Returns text. Use when receiving a voice message from the user."
endpoint: "http://localhost:9011/transcribe-url"
method: POST
parameters:
  audio_url: { type: string, required: true, location: body, description: "Audio file URL to transcribe" }
  language: { type: string, required: false, location: body, description: "Language code (ru, en, etc.)", default: "ru" }
response_transform: "$.text"
status: verified
"#;

const ANALYZE_IMAGE: &str = r#"
name: analyze_image
description: "Analyze an image from a URL or /uploads/ path. Accepts image_url and an optional question. Returns a text description. Works with both external URLs (https://...) and internal /uploads/ paths."
endpoint: "http://localhost:18789/api/vision/analyze"
method: POST
parameters:
  image_url: { type: string, required: true, location: body, description: "Image URL to analyze (external https:// or internal /uploads/ path)" }
  question: { type: string, required: false, location: body, description: "Question about the image (optional)", default: "Describe what is in the image" }
  language: { type: string, required: false, location: body, description: "Response language (default: ru)", default: "ru" }
response_transform: "$.description"
status: verified
"#;

pub fn capability_specs() -> &'static [CapabilitySpec] {
    &[
        CapabilitySpec { capability: "imagegen",  tool_name: "generate_image",    yaml: GENERATE_IMAGE },
        CapabilitySpec { capability: "tts",       tool_name: "synthesize_speech", yaml: SYNTHESIZE_SPEECH },
        CapabilitySpec { capability: "websearch", tool_name: "search_web",        yaml: SEARCH_WEB },
        CapabilitySpec { capability: "stt",       tool_name: "transcribe_audio",  yaml: TRANSCRIBE_AUDIO },
        CapabilitySpec { capability: "vision",    tool_name: "analyze_image",     yaml: ANALYZE_IMAGE },
    ]
}

pub fn parse_spec(spec: &CapabilitySpec) -> anyhow::Result<YamlToolDef> {
    Ok(serde_yaml::from_str(spec.yaml)?)
}

fn with_provider(mut def: YamlToolDef, provider: &str) -> YamlToolDef {
    def.description = format!("{} (provider: {provider})", def.description.trim());
    def
}

pub async fn capability_tool_defs(db: &sqlx::PgPool) -> Vec<YamlToolDef> {
    let mut out = Vec::new();
    for spec in capability_specs() {
        let top = match crate::db::providers::get_active_providers(db, spec.capability).await {
            Ok(list) => list.into_iter().next().map(|(name, _)| name),
            Err(e) => {
                tracing::warn!(capability = spec.capability, error = %e, "active providers query failed");
                None
            }
        };
        let Some(provider) = top else { continue };
        match parse_spec(spec) {
            Ok(def) => out.push(with_provider(def, &provider)),
            Err(e) => tracing::error!(tool = spec.tool_name, error = %e, "capability spec parse failed"),
        }
    }
    out
}

pub async fn find_capability_tool(db: &sqlx::PgPool, name: &str) -> Option<YamlToolDef> {
    let spec = capability_specs().iter().find(|s| s.tool_name == name)?;
    let top = crate::db::providers::get_active_providers(db, spec.capability)
        .await.ok()?.into_iter().next().map(|(n, _)| n)?;
    parse_spec(spec).ok().map(|d| with_provider(d, &top))
}

/// Разрешить имя инструмента в YamlToolDef: capability-имена зарезервированы
/// (приоритет над YAML-файлом), иначе — обычный YAML-инструмент.
pub async fn resolve_tool(
    workspace_dir: &str,
    db: &sqlx::PgPool,
    name: &str,
) -> Option<YamlToolDef> {
    if is_capability_tool(name) {
        return find_capability_tool(db, name).await;
    }
    crate::tools::yaml_tools::find_yaml_tool(workspace_dir, name).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_specs_parse_with_correct_names() {
        for spec in capability_specs() {
            let def = parse_spec(spec).unwrap_or_else(|e| panic!("{} failed: {e}", spec.tool_name));
            assert_eq!(def.name, spec.tool_name);
            assert!(!def.description.is_empty());
        }
        assert_eq!(capability_specs().len(), CAPABILITY_TOOL_NAMES.len());
    }

    #[test]
    fn search_web_has_no_provider_param() {
        let spec = capability_specs().iter().find(|s| s.tool_name == "search_web").unwrap();
        assert!(!parse_spec(spec).unwrap().parameters.contains_key("provider"));
    }

    #[test]
    fn tts_keeps_body_template_and_timeout() {
        let spec = capability_specs().iter().find(|s| s.tool_name == "synthesize_speech").unwrap();
        let def = parse_spec(spec).unwrap();
        assert_eq!(def.timeout, 600);
        let bt = def.body_template.as_deref().unwrap_or("");
        assert!(bt.contains("\"input\""), "TTS body must use 'input' key: {bt}");
    }

    #[test]
    fn binary_tools_have_channel_action() {
        for name in ["generate_image", "synthesize_speech"] {
            let spec = capability_specs().iter().find(|s| s.tool_name == name).unwrap();
            assert!(parse_spec(spec).unwrap().channel_action.is_some());
        }
    }

    #[cfg(test)]
    async fn seed_provider(pool: &sqlx::PgPool, name: &str, capability: &str, driver: &str) {
        sqlx::query("INSERT INTO providers (name, type, provider_type, enabled) VALUES ($1,$2,$3,true)")
            .bind(name).bind(capability).bind(driver)
            .execute(pool).await.unwrap();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn defs_include_active_provider_in_description(pool: sqlx::PgPool) {
        seed_provider(&pool, "flux-fal", "imagegen", "fal").await;
        crate::db::providers::set_provider_active_list(&pool, "imagegen", &[("flux-fal".into(), 1)])
            .await.unwrap();
        let defs = capability_tool_defs(&pool).await;
        let gi = defs.iter().find(|d| d.name == "generate_image").expect("generate_image present");
        assert!(gi.description.contains("flux-fal"), "desc must name provider: {}", gi.description);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn no_def_when_capability_has_no_active_provider(pool: sqlx::PgPool) {
        let defs = capability_tool_defs(&pool).await;
        assert!(defs.iter().all(|d| d.name != "generate_image"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_returns_top_priority_provider(pool: sqlx::PgPool) {
        seed_provider(&pool, "low", "tts", "edge").await;
        seed_provider(&pool, "top", "tts", "silero").await;
        crate::db::providers::set_provider_active_list(
            &pool, "tts", &[("low".into(), 10), ("top".into(), 1)]).await.unwrap();
        let def = find_capability_tool(&pool, "synthesize_speech").await.expect("found");
        assert!(def.description.contains("top"));
        assert!(!def.description.contains("low"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn find_is_none_without_provider(pool: sqlx::PgPool) {
        assert!(find_capability_tool(&pool, "search_web").await.is_none());
        assert!(find_capability_tool(&pool, "not_a_capability").await.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn tool_definition_description_carries_provider(pool: sqlx::PgPool) {
        seed_provider(&pool, "searxng", "websearch", "searxng").await;
        crate::db::providers::set_provider_active_list(&pool, "websearch", &[("searxng".into(), 1)])
            .await.unwrap();
        let td = find_capability_tool(&pool, "search_web").await.unwrap().to_tool_definition();
        assert_eq!(td.name, "search_web");
        assert!(td.description.contains("searxng"));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolve_prefers_capability(pool: sqlx::PgPool) {
        seed_provider(&pool, "p", "imagegen", "fal").await;
        crate::db::providers::set_provider_active_list(&pool, "imagegen", &[("p".into(), 1)])
            .await.unwrap();
        let def = resolve_tool("/nonexistent-workspace", &pool, "generate_image").await.unwrap();
        assert_eq!(def.name, "generate_image");
        assert!(def.description.contains("(provider: p)"));
    }
}
