//! Non-streaming response types for OpenAI-compatible chat completions.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct ChatCompletionResponse {
    #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub(super) choices: Vec<ChatChoice>,
    pub(super) usage: Option<ChatUsage>,
}

fn deserialize_null_as_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(std::option::Option::unwrap_or_default)
}

#[derive(Debug, Deserialize)]
pub(super) struct ChatChoice {
    pub(super) message: ChatMessage,
    pub(super) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ChatMessage {
    pub(super) content: Option<String>,
    pub(super) tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ChatToolCall {
    pub(super) id: String,
    pub(super) function: ChatFunction,
}

#[derive(Debug, Deserialize)]
pub(super) struct ChatFunction {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ChatUsage {
    pub(super) prompt_tokens: u32,
    pub(super) completion_tokens: u32,
    #[serde(default)]
    pub(super) prompt_tokens_details: Option<ChatPromptTokensDetails>,
    #[serde(default)]
    pub(super) completion_tokens_details: Option<ChatCompletionTokensDetails>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ChatPromptTokensDetails {
    #[serde(default)]
    pub(super) cached_tokens: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ChatCompletionTokensDetails {
    #[serde(default)]
    pub(super) reasoning_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_null_as_empty_vec_handles_null() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(deserialize_with = "deserialize_null_as_empty_vec")]
            items: Vec<String>,
        }
        let h: Holder = serde_json::from_str(r#"{"items": null}"#).unwrap();
        assert!(h.items.is_empty());
    }

    #[test]
    fn deserialize_null_as_empty_vec_handles_array() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(deserialize_with = "deserialize_null_as_empty_vec")]
            items: Vec<String>,
        }
        let h: Holder = serde_json::from_str(r#"{"items": ["a", "b"]}"#).unwrap();
        assert_eq!(h.items, vec!["a", "b"]);
    }

    #[test]
    fn openai_chat_usage_maps_cached_and_reasoning() {
        let json = r#"{
            "choices": [{
                "message": {"content": "hi", "tool_calls": null},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 600,
                "prompt_tokens_details": { "cached_tokens": 700 },
                "completion_tokens_details": { "reasoning_tokens": 400 }
            }
        }"#;
        let resp: ChatCompletionResponse = serde_json::from_str(json).expect("parse");
        let u = resp.usage.expect("usage present");

        assert_eq!(u.prompt_tokens, 1000);
        assert_eq!(u.completion_tokens, 600);
        assert_eq!(
            u.prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            Some(700)
        );
        assert_eq!(
            u.completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens),
            Some(400)
        );
    }
}
