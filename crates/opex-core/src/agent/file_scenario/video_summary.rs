//! Builds the Obsidian note and LLM digest prompt from toolgate raw material.
//! The whole transcript goes in (large context — no telesumbot 40k chunking).
//! Prompts ported from telesumbot `summary/prompts.rs`.

use serde::Deserialize;
use opex_types::{Message, MessageRole};

#[derive(Debug, Clone, Deserialize)]
pub struct FrameDesc {
    pub timestamp: f64,
    pub description: String,
    #[serde(default)]
    pub image_b64: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Degraded {
    /// Tracked in API response; not yet read by the digest builder (v1 uses transcript regardless).
    #[allow(dead_code)]
    pub stt: bool,
    pub vision: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawMaterial {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub duration: f64,
    pub transcript: String,
    #[serde(default)]
    pub frames: Vec<FrameDesc>,
    #[serde(default)]
    pub degraded: Degraded,
}

const SYSTEM_PROMPT: &str = "Ты помощник, который делает структурированный русскоязычный \
конспект видео по его транскрипту и описаниям ключевых кадров. \
Выведи ДВА раздела, точно в таком формате (ничего лишнего до первого раздела):\n\
\n\
## Резюме\n\
<3-5 предложений, суть видео>\n\
\n\
## Конспект\n\
<структурированный конспект с подзаголовками ### и таймкодами>\n\
\n\
Там, где уместно, вставляй изображения из списка кадров, используя ТОЧНО такой синтаксис: \
![[_System/media/<имя_файла>]]\n\
Пиши по-русски, без воды.";

/// Filesystem/Obsidian-safe slug; keeps Cyrillic, strips specials, spaces→'-'.
pub fn slug(title: &str, fallback_id: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c => c,
        })
        .collect();
    let s = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { format!("видео-{fallback_id}") } else { s }
}

/// Build the system+user messages for the digest. The entire transcript is
/// embedded (large-context model — no chunking). `frame_names` are the
/// filenames that will be saved to `_System/media/` so the LLM can embed them.
pub fn build_summary_messages(raw: &RawMaterial, frame_names: &[String]) -> Vec<Message> {
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
        user.push_str("=== Ключевые кадры (таймкод → описание → embed-строка) ===\n");
        for (f, name) in raw.frames.iter().zip(frame_names.iter()) {
            user.push_str(&format!(
                "[{:.0}s] {} → ![[_System/media/{}]]\n",
                f.timestamp, f.description, name
            ));
        }
        // Frames without a corresponding name (shouldn't happen, but be safe).
        if raw.frames.len() > frame_names.len() {
            for f in raw.frames.iter().skip(frame_names.len()) {
                user.push_str(&format!("[{:.0}s] {}\n", f.timestamp, f.description));
            }
        }
    }
    user.push_str("\nСделай конспект по инструкции.");

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

/// Build the full Obsidian note: frontmatter + LLM body + unplaced-frame appendix
/// + collapsed transcript.
///
/// Deterministic — does NOT call `Utc::now()`. The worker (Task 6) prepends the
/// `created` date field before writing.
pub fn build_note(raw: &RawMaterial, title: &str, llm_body: &str, frame_names: &[String]) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("title: {title}\n"));
    out.push_str("tags: [видео, конспект]\n");
    out.push_str(&format!("duration: {:.0}s\n", raw.duration));
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(llm_body.trim());
    out.push('\n');

    // Appendix: frames whose embed string the LLM did not include.
    let unplaced: Vec<&String> = frame_names.iter()
        .filter(|n| !llm_body.contains(n.as_str()))
        .collect();
    if !unplaced.is_empty() {
        out.push_str("\n## Дополнительные кадры\n\n");
        for n in unplaced {
            out.push_str(&format!("![[_System/media/{n}]]\n\n"));
        }
    }
    // Collapsed full transcript.
    out.push_str("\n> [!note]- Полный транскрипт\n");
    for line in raw.transcript.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The text under `## Резюме` up to the next `## `; else the first paragraph.
pub fn extract_summary(note: &str) -> String {
    if let Some(start) = note.find("## Резюме") {
        let after = &note[start + "## Резюме".len()..];
        let body = after.split("\n## ").next().unwrap_or(after);
        return body.trim().to_string();
    }
    note.split("\n\n").map(str::trim).find(|p| !p.is_empty()).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::MessageRole;

    #[test]
    fn slug_keeps_cyrillic_strips_specials() {
        assert_eq!(slug("Лекция: Rust / async?", "id8"), "Лекция-Rust-async");
        assert_eq!(slug("   ", "ab12cd34"), "видео-ab12cd34");
    }

    #[test]
    fn build_note_has_frontmatter_appendix_and_transcript() {
        let raw = RawMaterial {
            title: Some("Тест".into()), duration: 65.0, transcript: "речь целиком".into(),
            frames: vec![
                FrameDesc { timestamp: 5.0, description: "слайд".into(), image_b64: "x".into() },
                FrameDesc { timestamp: 9.0, description: "график".into(), image_b64: "y".into() },
            ],
            degraded: Degraded::default(),
        };
        let names = vec!["t-frame-01.jpg".to_string(), "t-frame-02.jpg".to_string()];
        // LLM used only frame 1 inline; frame 2 must go to appendix.
        let llm_body = "## Резюме\nкоротко\n\n## Конспект\n### Раздел\n![[_System/media/t-frame-01.jpg]]\n";
        let note = build_note(&raw, "Тест", llm_body, &names);
        assert!(note.starts_with("---\n"), "frontmatter");
        assert!(note.contains("title: Тест"));
        assert!(note.contains("![[_System/media/t-frame-01.jpg]]"));
        assert!(note.contains("## Дополнительные кадры"));
        assert!(note.contains("![[_System/media/t-frame-02.jpg]]"), "unplaced frame appended");
        assert!(note.contains("> [!note]- Полный транскрипт"));
        assert!(note.contains("речь целиком"));
    }

    #[test]
    fn extract_summary_reads_section_or_falls_back() {
        let note = "---\nx\n---\n## Резюме\nэто резюме\n\n## Конспект\nтело\n";
        assert_eq!(extract_summary(note).trim(), "это резюме");
        let no_section = "просто первый абзац\n\nвторой";
        assert_eq!(extract_summary(no_section).trim(), "просто первый абзац");
    }

    #[test]
    fn prompt_embeds_transcript_and_frames() {
        let raw = RawMaterial {
            title: None,
            duration: 90.0,
            transcript: "полный текст речи".into(),
            frames: vec![FrameDesc { timestamp: 12.5, description: "синий слайд".into(), image_b64: String::new() }],
            degraded: Degraded::default(),
        };
        let frame_names = vec!["frame-01.jpg".to_string()];
        let msgs = build_summary_messages(&raw, &frame_names);
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
            title: None,
            duration: 10.0,
            transcript: "речь".into(),
            frames: vec![],
            degraded: Degraded { stt: false, vision: true },
        };
        let msgs = build_summary_messages(&raw, &[]);
        let user = &msgs[msgs.len() - 1];
        assert!(user.content.contains("без кадров") || user.content.contains("кадры недоступны"),
            "degraded vision is noted to the model");
    }
}
