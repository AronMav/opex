//! Provider type registry: the static catalog of every named connection
//! provider we know how to talk to (`openai`, `anthropic`, `google`,
//! `claude-cli`, plus 20+ OpenAI-compatible vendors).
//!
//! `PROVIDER_TYPES` is consumed by:
//!
//! - `factory::build_provider` to dispatch on `row.provider_type`,
//! - the `*_impl` providers to look up `default_secret_name` /
//!   `chat_path` / `default_base_url`,
//! - the gateway `/api/providers/types` handler which serializes
//!   `ProviderTypeMeta` as the canonical type list to the UI,
//! - `resolve_chat_url` / `default_base_url_for_type` helpers.
//!
//! Adding a new OpenAI-compatible vendor: append a `ProviderTypeMeta` to
//! the table — no other code changes needed unless the vendor needs a
//! non-standard wire format.

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderTypeMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub chat_path: &'static str,
    pub default_base_url: &'static str,
    pub default_secret_name: &'static str,
    pub requires_api_key: bool,
    pub supports_model_listing: bool,
    /// For CLI providers: delegate model listing to this provider type's API
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_provider: Option<&'static str>,
    /// Hardcoded fallback models when runtime fetch fails
    pub default_models: &'static [&'static str],
}

/// Known provider types with extended metadata.
pub(crate) const PROVIDER_TYPES: &[ProviderTypeMeta] = &[
    ProviderTypeMeta {
        id: "minimax",
        name: "MiniMax",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.minimax.io",
        default_secret_name: "MINIMAX_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "openai",
        name: "OpenAI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.openai.com",
        default_secret_name: "OPENAI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "anthropic",
        name: "Anthropic",
        chat_path: "",
        default_base_url: "https://api.anthropic.com",
        default_secret_name: "ANTHROPIC_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "google",
        name: "Google Gemini",
        chat_path: "",
        default_base_url: "https://generativelanguage.googleapis.com",
        default_secret_name: "GOOGLE_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "deepseek",
        name: "DeepSeek",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.deepseek.com",
        default_secret_name: "DEEPSEEK_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "groq",
        name: "Groq",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.groq.com/openai",
        default_secret_name: "GROQ_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "openrouter",
        name: "OpenRouter",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://openrouter.ai/api",
        default_secret_name: "OPENROUTER_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "mistral",
        name: "Mistral",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.mistral.ai",
        default_secret_name: "MISTRAL_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "xai",
        name: "xAI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.x.ai",
        default_secret_name: "XAI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "perplexity",
        name: "Perplexity",
        chat_path: "/chat/completions",
        default_base_url: "https://api.perplexity.ai",
        default_secret_name: "PERPLEXITY_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "together",
        name: "Together AI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.together.xyz",
        default_secret_name: "TOGETHER_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "ollama",
        name: "Ollama",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:11434",
        default_secret_name: "",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "openai_compat",
        name: "OpenAI Compatible",
        chat_path: "/v1/chat/completions",
        default_base_url: "",
        default_secret_name: "API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "claude-cli",
        name: "Claude CLI",
        chat_path: "",
        default_base_url: "",
        default_secret_name: "ANTHROPIC_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: Some("anthropic"),
        default_models: &["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"],
    },
    ProviderTypeMeta {
        id: "gemini-cli",
        name: "Gemini CLI",
        chat_path: "",
        default_base_url: "",
        default_secret_name: "GEMINI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: Some("google"),
        default_models: &["gemini-3.1-pro-preview", "gemini-3-flash-preview", "gemini-2.5-flash", "gemini-2.5-pro"],
    },
    ProviderTypeMeta {
        id: "codex-cli",
        name: "Codex CLI",
        chat_path: "",
        default_base_url: "",
        default_secret_name: "OPENAI_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: Some("openai"),
        default_models: &["codex-mini", "gpt-4.1", "o4-mini"],
    },
    // ── Additional OpenAI-compatible providers ──────────────────────────────
    ProviderTypeMeta {
        id: "huggingface",
        name: "Hugging Face",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api-inference.huggingface.co",
        default_secret_name: "HF_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "moonshot",
        name: "Moonshot AI (Kimi)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.moonshot.cn",
        default_secret_name: "MOONSHOT_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "nvidia",
        name: "NVIDIA",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://integrate.api.nvidia.com",
        default_secret_name: "NVIDIA_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "venice",
        name: "Venice AI",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.venice.ai",
        default_secret_name: "VENICE_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "cloudflare",
        name: "Cloudflare AI Gateway",
        chat_path: "/v1/chat/completions",
        default_base_url: "",
        default_secret_name: "CF_AI_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "litellm",
        name: "LiteLLM",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:4000",
        default_secret_name: "LITELLM_API_KEY",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "volcengine",
        name: "Volcengine (Doubao)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://ark.cn-beijing.volces.com/api",
        default_secret_name: "VOLCENGINE_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "qwen",
        name: "Qwen (Alibaba)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://dashscope.aliyuncs.com/compatible-mode",
        default_secret_name: "DASHSCOPE_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "glm",
        name: "GLM (Zhipu AI)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://open.bigmodel.cn/api/paas",
        default_secret_name: "GLM_API_KEY",
        requires_api_key: true,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "sglang",
        name: "SGLang",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:30000",
        default_secret_name: "",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "vllm",
        name: "vLLM",
        chat_path: "/v1/chat/completions",
        default_base_url: "http://localhost:8000",
        default_secret_name: "",
        requires_api_key: false,
        supports_model_listing: true,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "qianfan",
        name: "Qianfan (Baidu)",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://qianfan.baidubce.com",
        default_secret_name: "QIANFAN_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
    ProviderTypeMeta {
        id: "xiaomi",
        name: "Xiaomi MiLM",
        chat_path: "/v1/chat/completions",
        default_base_url: "https://api.ai.xiaomi.com",
        default_secret_name: "XIAOMI_API_KEY",
        requires_api_key: true,
        supports_model_listing: false,
        models_provider: None,
        default_models: &[],
    },
];

/// Metadata for the `gemini-cloudcode` provider. Declared as a separate
/// feature-gated const (instead of a `PROVIDER_TYPES` element) because
/// Rust stable does not support per-element `#[cfg]` inside a const array
/// literal. Helper functions (`resolve_chat_url`, `default_base_url_for_type`)
/// check this const with a feature-gated early return before the main scan.
#[cfg(feature = "gemini-cloudcode")]
pub(crate) const GEMINI_CLOUD_CODE_META: ProviderTypeMeta = ProviderTypeMeta {
    id: "gemini-cloudcode",
    name: "Google Gemini (Code Assist OAuth)",
    chat_path: "", // provider builds URLs internally from base_url
    default_base_url: "https://cloudcode-pa.googleapis.com",
    default_secret_name: "", // OAuth-based, no env secret
    requires_api_key: false,
    supports_model_listing: false, // static model list for MVP
    models_provider: None,
    default_models: &["gemini-2.5-pro", "gemini-2.5-flash", "gemini-3.1-pro-preview"],
};

/// Build full chat completions URL from `base_url` + provider's `chat_path`.
/// True when the base URL's final path segment is a version marker (`v1`, `v4`,
/// `v1beta`, …). OpenAI-style suffixes (`/v1/chat/completions`, `/v1/models`)
/// must then be appended WITHOUT the extra `/v1` — the base already carries the
/// version (e.g. z.ai's `.../paas/v4` serves chat at `{base}/chat/completions`,
/// not `{base}/v1/chat/completions`).
pub fn base_url_has_version(base_url: &str) -> bool {
    let seg = base_url.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let mut chars = seg.chars();
    matches!(chars.next(), Some('v') | Some('V')) && chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// Join a base URL with an OpenAI-style `/v1/...` suffix, dropping the leading
/// `/v1` when the base already ends in a version segment. The single source of
/// truth for building any OpenAI-compatible endpoint (chat, models, model probe)
/// so a versioned base (e.g. z.ai's `.../paas/v4`) never double-versions.
pub fn join_openai_path(base_url: &str, v1_suffix: &str) -> String {
    let suffix = if base_url_has_version(base_url) && v1_suffix.starts_with("/v1/") {
        &v1_suffix[3..]
    } else {
        v1_suffix
    };
    format!("{}{}", base_url.trim_end_matches('/'), suffix)
}

pub fn resolve_chat_url(provider_type: &str, base_url: &str) -> String {
    // Feature-gated early return: gemini-cloudcode uses an empty chat_path,
    // so the caller's base_url is used as-is (same logic as the empty-path
    // branch below, but avoids splicing a cfg-gated entry into PROVIDER_TYPES).
    #[cfg(feature = "gemini-cloudcode")]
    if provider_type == GEMINI_CLOUD_CODE_META.id {
        return base_url.to_string();
    }

    let chat_path = PROVIDER_TYPES.iter()
        .find(|pt| pt.id == provider_type)
        .map_or("/v1/chat/completions", |pt| pt.chat_path);
    if chat_path.is_empty() {
        return base_url.to_string();
    }
    join_openai_path(base_url, chat_path)
}

/// Default base URL for a provider type (from `PROVIDER_TYPES`).
#[allow(dead_code)] // Public API surface — kept for stability across plugin boundaries.
pub fn default_base_url_for_type(provider_type: &str) -> &'static str {
    // Feature-gated early return: gemini-cloudcode is not in PROVIDER_TYPES.
    #[cfg(feature = "gemini-cloudcode")]
    if provider_type == GEMINI_CLOUD_CODE_META.id {
        return GEMINI_CLOUD_CODE_META.default_base_url;
    }

    PROVIDER_TYPES.iter()
        .find(|pt| pt.id == provider_type)
        .map_or("", |pt| pt.default_base_url)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod chat_url_tests {
    use super::*;

    #[test]
    fn versioned_base_drops_v1() {
        // z.ai: base already carries /paas/v4 → chat is {base}/chat/completions.
        assert_eq!(
            resolve_chat_url("openai_compat", "https://api.z.ai/api/coding/paas/v4"),
            "https://api.z.ai/api/coding/paas/v4/chat/completions"
        );
    }

    #[test]
    fn root_base_keeps_v1() {
        assert_eq!(
            resolve_chat_url("openai_compat", "https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            resolve_chat_url("openai", "https://token-plan-sgp.xiaomimimo.com"),
            "https://token-plan-sgp.xiaomimimo.com/v1/chat/completions"
        );
    }

    #[test]
    fn version_detection() {
        assert!(base_url_has_version("https://api.z.ai/api/coding/paas/v4"));
        assert!(base_url_has_version("https://x.com/v1"));
        assert!(base_url_has_version("https://x.com/v1beta/"));
        assert!(!base_url_has_version("https://api.openai.com"));
        assert!(!base_url_has_version("https://ollama.com/"));
    }
}

#[cfg(all(test, feature = "gemini-cloudcode"))]
mod tests {
    use super::*;

    // ── registry::tests::gemini_cloudcode_in_provider_types ─────────────────
    #[test]
    fn gemini_cloudcode_in_provider_types() {
        let entry = &GEMINI_CLOUD_CODE_META;
        assert_eq!(entry.id, "gemini-cloudcode");
        assert_eq!(entry.name, "Google Gemini (Code Assist OAuth)");
        assert_eq!(entry.default_base_url, "https://cloudcode-pa.googleapis.com");
        assert!(!entry.requires_api_key, "gemini-cloudcode uses OAuth, not an API key");
        assert!(!entry.supports_model_listing, "model listing is static in MVP");
        assert!(entry.default_models.contains(&"gemini-2.5-pro"));
        assert!(entry.default_models.contains(&"gemini-2.5-flash"));
    }
}
