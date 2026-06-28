//! Builds the Obsidian note and LLM digest prompt from toolgate raw material.
//! The whole transcript goes in (large context — no telesumbot 40k chunking).
//! Prompts ported from telesumbot `summary/prompts.rs`.

use serde::Deserialize;
use opex_types::{Message, MessageRole};

#[derive(Debug, Clone, Deserialize)]
pub struct FrameDesc {
    /// Present in the toolgate response; no longer read since screenshots were
    /// dropped (only `description` feeds the digest as on-screen context).
    #[allow(dead_code)]
    #[serde(default)]
    pub timestamp: f64,
    pub description: String,
    #[allow(dead_code)]
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
<ПОДРОБНЫЙ пошаговый конспект с подзаголовками ###>\n\
\n\
НЕ добавляй таймкоды или тайминги (например «[00:00]», «5:30», «(2:15)») нигде в конспекте — \
ни в заголовки ###, ни в пункты. Конспект должен быть чистым связным текстом без таймингов.\n\
\n\
КРИТИЧЕСКИ ВАЖНО — ПОДРОБНОСТЬ: каждый раздел ## Конспекта должен быть РАЗВЁРНУТЫМ — несколько \
пунктов списком (-) или полноценный абзац, а НЕ одна короткая строка-аннотация. В каждом разделе \
изложи ВСЕ технические детали из транскрипта: точную последовательность действий, названия \
инструментов/плагинов/функций/кнопок, горячие клавиши, числовые значения и настройки (частоты, BPM, \
проценты, дБ), важные нюансы и причины («зачем так делается»). Пиши настолько подробно, чтобы по \
конспекту можно было ПОВТОРИТЬ каждый шаг урока БЕЗ просмотра видео. НЕ опускай практические детали, \
приёмы и второстепенные советы ради краткости — лучше длиннее и полнее, чем коротко.\n\
\n\
Тебе также даны описания того, ЧТО ПОКАЗАНО НА ЭКРАНЕ в ключевые моменты (окна плагинов, \
панели настроек, значения параметров). Используй их, чтобы точнее и подробнее изложить детали \
в ТЕКСТЕ конспекта (точные значения, названия окон/параметров, что именно видно). \
НЕ вставляй в конспект изображения, кадры или embed-строки вида ![](...) — только текст.\n\
\n\
Пиши по-русски, без воды.";

/// Filesystem/Obsidian-safe slug; keeps Cyrillic, strips specials, spaces→'-'.
pub fn slug(title: &str, fallback_id: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            // '#' stripped too — it breaks Obsidian `[[wikilink#section]]` parsing.
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' => ' ',
            c => c,
        })
        .collect();
    let s = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { format!("видео-{fallback_id}") } else { s }
}

/// Strip leading `[MM:SS]` / `[MMM:SS]` timecode markers (carried by the
/// timestamped transcript) so the DIGEST PROMPT sees plain text and the LLM does
/// not copy timecodes into the conspect body. The stored transcript in the note
/// keeps its markers for navigation — only the prompt copy is stripped.
fn strip_transcript_timecodes(text: &str) -> String {
    text.lines()
        .map(|line| {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix('[')
                && let Some(close) = rest.find(']')
                && let Some((m, s)) = rest[..close].split_once(':')
                && !m.is_empty()
                && m.bytes().all(|b| b.is_ascii_digit())
                && s.len() == 2
                && s.bytes().all(|b| b.is_ascii_digit())
            {
                return rest[close + 1..].trim_start().to_string();
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the system+user messages for the digest. The entire transcript is
/// embedded (large-context model — no chunking). Frame DESCRIPTIONS are passed as
/// on-screen context to enrich the TEXT; screenshots are no longer embedded.
pub fn build_summary_messages(raw: &RawMaterial) -> Vec<Message> {
    let mut user = String::new();
    user.push_str(&format!("Длительность видео: {:.0} сек.\n\n", raw.duration));
    user.push_str("=== Транскрипт ===\n");
    // Strip timecodes from the prompt copy so the LLM does not reproduce them.
    user.push_str(&strip_transcript_timecodes(&raw.transcript));
    user.push_str("\n\n");

    if raw.frames.is_empty() {
        if raw.degraded.vision {
            user.push_str("(Описания экрана недоступны — vision-провайдер не активен; \
                           сделай сводку только по транскрипту.)\n");
        }
    } else {
        user.push_str("=== Что показано на экране в ключевые моменты (для деталей в тексте) ===\n");
        for f in &raw.frames {
            user.push_str(&format!("- {}\n", f.description));
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

/// Remove every image embed line (`![](images/...)`) from `body`. Screenshots are
/// no longer included in the note — full-frame screencap thumbnails did not help
/// the reader (arbitrary moment, uncropped, redundant with the text). The frame
/// DESCRIPTIONS still feed the digest prompt so on-screen content reaches the text;
/// only the images themselves are dropped. This also strips any embed the LLM
/// hallucinated, leaving a clean text conspect.
fn strip_image_embeds(body: &str) -> String {
    body.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with("![](images/") && t.ends_with(')'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the full Obsidian note: frontmatter + LLM body (text only, no screenshots)
/// + collapsed transcript.
///
/// Deterministic — does NOT call `Utc::now()`. The worker (Task 6) prepends the
/// `created` date field before writing.
pub fn build_note(raw: &RawMaterial, title: &str, llm_body: &str) -> String {
    let body = strip_image_embeds(llm_body.trim());

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("title: {title}\n"));
    out.push_str("tags: [видео, конспект]\n");
    out.push_str(&format!("duration: {:.0}s\n", raw.duration));
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(body.trim());
    out.push('\n');

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
    // No ## Резюме — skip a leading YAML frontmatter block, then first real paragraph.
    let body = if note.starts_with("---") {
        note.splitn(3, "---").nth(2).unwrap_or(note)
    } else {
        note
    };
    body.split("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty() && !p.starts_with('#') && !p.starts_with("---"))
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::MessageRole;

    #[test]
    fn slug_keeps_cyrillic_strips_specials() {
        assert_eq!(slug("Лекция: Rust / async?", "id8"), "Лекция-Rust-async");
        assert_eq!(slug("   ", "ab12cd34"), "видео-ab12cd34");
        assert_eq!(slug("Урок #5", "x"), "Урок-5", "'#' stripped (Obsidian wikilink-safe)");
    }

    #[test]
    fn build_note_is_text_only_strips_all_image_embeds() {
        let raw = RawMaterial {
            title: Some("Тест".into()), duration: 65.0, transcript: "речь целиком".into(),
            frames: vec![
                FrameDesc { timestamp: 5.0, description: "слайд".into(), image_b64: "x".into() },
            ],
            degraded: Degraded::default(),
        };
        // The LLM embedded a real frame AND hallucinated one — BOTH must be removed.
        let llm_body = "## Резюме\nкоротко\n\n## Конспект\n### Раздел\nтекст\n![](images/frame-01.jpg)\n\n![](images/frame-99.jpg)\n\nещё абзац";
        let note = build_note(&raw, "Тест", llm_body);
        assert!(note.starts_with("---\n"), "frontmatter");
        assert!(!note.contains("![](images/"), "no image embeds at all — screenshots removed");
        assert!(!note.contains("## Дополнительные кадры"), "no frame appendix");
        // Real text is kept.
        assert!(note.contains("коротко") && note.contains("ещё абзац"), "body text kept");
        assert!(note.contains("> [!note]- Полный транскрипт") && note.contains("речь целиком"));
    }

    #[test]
    fn extract_summary_reads_section_or_falls_back() {
        let note = "---\nx\n---\n## Резюме\nэто резюме\n\n## Конспект\nтело\n";
        assert_eq!(extract_summary(note).trim(), "это резюме");
        let no_section = "просто первый абзац\n\nвторой";
        assert_eq!(extract_summary(no_section).trim(), "просто первый абзац");
    }

    /// I3: fallback must skip frontmatter and H1 heading, return first real paragraph.
    #[test]
    fn extract_summary_fallback_skips_heading_and_frontmatter() {
        let note = "---\nx\n---\n# Заголовок\n\nреальный текст\n";
        assert_eq!(
            extract_summary(note).trim(),
            "реальный текст",
            "fallback should skip heading and return first real paragraph"
        );
    }

    #[test]
    fn prompt_has_transcript_and_frame_descriptions_no_embeds() {
        let raw = RawMaterial {
            title: None,
            duration: 90.0,
            transcript: "полный текст речи".into(),
            frames: vec![FrameDesc { timestamp: 12.5, description: "синий слайд".into(), image_b64: String::new() }],
            degraded: Degraded::default(),
        };
        let msgs = build_summary_messages(&raw);
        assert_eq!(msgs[0].role, MessageRole::System);
        let user = &msgs[msgs.len() - 1];
        assert_eq!(user.role, MessageRole::User);
        assert!(user.content.contains("полный текст речи"), "whole transcript embedded");
        assert!(user.content.contains("синий слайд"), "frame description passed as on-screen context");
        assert!(!user.content.contains("![](images/"), "no embed strings in the prompt");
    }

    #[test]
    fn digest_prompt_strips_transcript_timecodes() {
        // The timestamped transcript must NOT carry [MM:SS] into the digest prompt
        // (else the LLM copies them into the conspect body). The stored note keeps them.
        assert_eq!(
            strip_transcript_timecodes("[00:04] раз\n[131:20] два\nбез метки"),
            "раз\nдва\nбез метки"
        );
        let raw = RawMaterial {
            title: None, duration: 60.0,
            transcript: "[00:04] начало\n[01:30] середина".into(),
            frames: vec![], degraded: Degraded::default(),
        };
        let user = &build_summary_messages(&raw)[1].content;
        assert!(user.contains("начало") && user.contains("середина"), "text kept");
        assert!(!user.contains("[00:04]") && !user.contains("[01:30]"), "no timecodes in prompt");
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
        let msgs = build_summary_messages(&raw);
        let user = &msgs[msgs.len() - 1];
        assert!(user.content.contains("Описания экрана недоступны") || user.content.contains("только по транскрипту"),
            "degraded vision is noted to the model");
    }
}
