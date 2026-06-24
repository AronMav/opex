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
}
