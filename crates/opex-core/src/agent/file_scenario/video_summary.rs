//! Builds the final LLM digest prompt from toolgate raw material. The whole
//! transcript goes in (large context — no telesumbot 40k chunking). Prompts
//! ported from telesumbot `summary/prompts.rs`.

use serde::Deserialize;
use opex_types::{Message, MessageRole};

#[derive(Debug, Clone, Deserialize)]
pub struct FrameDesc {
    pub timestamp: f64,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Degraded {
    pub stt: bool,
    pub vision: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawMaterial {
    #[serde(default)]
    pub duration: f64,
    pub transcript: String,
    #[serde(default)]
    pub frames: Vec<FrameDesc>,
    #[serde(default)]
    pub degraded: Degraded,
}

const SYSTEM_PROMPT: &str = "Ты помощник, который делает структурированную русскоязычную \
сводку видео по его транскрипту и описаниям ключевых кадров. Дай: краткое резюме (3-5 \
предложений), затем основные тезисы списком с таймкодами, затем выводы. Пиши по-русски, \
без воды.";

/// Build the system+user messages for the digest. The entire transcript is
/// embedded (large-context model — no chunking).
pub fn build_summary_messages(raw: &RawMaterial) -> Vec<Message> {
    let mut user = String::new();
    user.push_str(&format!("Длительность видео: {:.0} сек.\n\n", raw.duration));
    user.push_str("=== Транскрипт ===\n");
    user.push_str(&raw.transcript);
    user.push_str("\n\n");

    if raw.frames.is_empty() {
        if raw.degraded.vision {
            user.push_str("(Описания кадров недоступны — vision-провайдер не активен; \
                           сделай сводку без кадров.)\n");
        }
    } else {
        user.push_str("=== Ключевые кадры (таймкод → описание) ===\n");
        for f in &raw.frames {
            user.push_str(&format!("[{:.0}s] {}\n", f.timestamp, f.description));
        }
    }
    user.push_str("\nСделай сводку по инструкции.");

    vec![
        Message {
            role: MessageRole::System,
            content: SYSTEM_PROMPT.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        },
        Message {
            role: MessageRole::User,
            content: user,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::MessageRole;

    #[test]
    fn prompt_embeds_transcript_and_frames() {
        let raw = RawMaterial {
            duration: 90.0,
            transcript: "полный текст речи".into(),
            frames: vec![FrameDesc { timestamp: 12.5, description: "синий слайд".into() }],
            degraded: Degraded::default(),
        };
        let msgs = build_summary_messages(&raw);
        assert_eq!(msgs[0].role, MessageRole::System);
        let user = &msgs[msgs.len() - 1];
        assert_eq!(user.role, MessageRole::User);
        assert!(user.content.contains("полный текст речи"), "whole transcript embedded");
        assert!(user.content.contains("синий слайд"), "frame description embedded");
        assert!(user.content.contains("12"), "timestamp embedded");
    }

    #[test]
    fn degraded_vision_note_present() {
        let raw = RawMaterial {
            duration: 10.0,
            transcript: "речь".into(),
            frames: vec![],
            degraded: Degraded { stt: false, vision: true },
        };
        let msgs = build_summary_messages(&raw);
        let user = &msgs[msgs.len() - 1];
        assert!(user.content.contains("без кадров") || user.content.contains("кадры недоступны"),
            "degraded vision is noted to the model");
    }
}
