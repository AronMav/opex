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
<ПОДРОБНЫЙ пошаговый конспект с подзаголовками ### и таймкодами>\n\
\n\
КРИТИЧЕСКИ ВАЖНО — ПОДРОБНОСТЬ: каждый раздел ## Конспекта должен быть РАЗВЁРНУТЫМ — несколько \
пунктов списком (-) или полноценный абзац, а НЕ одна короткая строка-аннотация. В каждом разделе \
изложи ВСЕ технические детали из транскрипта: точную последовательность действий, названия \
инструментов/плагинов/функций/кнопок, горячие клавиши, числовые значения и настройки (частоты, BPM, \
проценты, дБ), важные нюансы и причины («зачем так делается»). Пиши настолько подробно, чтобы по \
конспекту можно было ПОВТОРИТЬ каждый шаг урока БЕЗ просмотра видео. НЕ опускай практические детали, \
приёмы и второстепенные советы ради краткости — лучше длиннее и полнее, чем коротко.\n\
\n\
Тебе даны кадры видео с таймкодами и описаниями. После КАЖДОГО отдельного тезиса/пункта, \
к которому кадр относится по таймкоду и смыслу, вставь РОВНО ОДНУ embed-строку этого кадра. \
КАТЕГОРИЧЕСКИ НЕ группируй несколько кадров подряд (две и более embed-строки вплотную — запрещено). \
Размещай кадры ПО ОДНОМУ, разнося их по разным пунктам и разделам конспекта. \
Каждый предоставленный кадр должен появиться в теле ровно один раз; используй ВСЕ кадры.\n\
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
                "[{:.0}s] {} → ![](images/{})\n",
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

/// Insert the embed lines for frames the LLM did NOT place inline, distributing
/// them through `body` by each frame's own timestamp (the conspect text follows
/// the video chronologically, so timestamp position maps roughly to text
/// position). This avoids the failure mode where the LLM — especially on long
/// transcripts — ignores the "embed a frame after each thesis" instruction and
/// leaves ALL frames to be dumped in a trailing appendix. Frames the LLM already
/// embedded keep their position. Does NOT rely on the LLM writing section
/// timecodes; uses `raw.frames[i].timestamp` directly.
fn distribute_unplaced_frames(body: &str, raw: &RawMaterial, frame_names: &[String]) -> String {
    let blocks: Vec<&str> = body.split("\n\n").collect();
    if blocks.is_empty() {
        return body.to_string();
    }
    let dur = if raw.duration > 0.0 { raw.duration } else { 1.0 };
    // Trailing embeds to append after each block.
    let mut inserts: Vec<Vec<String>> = vec![Vec::new(); blocks.len()];
    for (i, name) in frame_names.iter().enumerate() {
        if body.contains(name.as_str()) {
            continue; // already placed inline by the LLM — leave it
        }
        let frac = raw
            .frames
            .get(i)
            .map(|f| (f.timestamp / dur).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let mut idx = ((frac * blocks.len() as f64) as usize).min(blocks.len() - 1);
        // Spread: if the target block already holds an embed, walk forward
        // (wrapping) so two frames don't pile onto the same spot.
        let start = idx;
        while !inserts[idx].is_empty() {
            idx = (idx + 1) % blocks.len();
            if idx == start {
                break;
            }
        }
        inserts[idx].push(format!("![](images/{name})"));
    }
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        out.push_str(block);
        for emb in &inserts[i] {
            out.push_str("\n\n");
            out.push_str(emb);
        }
        if i + 1 < blocks.len() {
            out.push_str("\n\n");
        }
    }
    out
}

/// Build the full Obsidian note: frontmatter + LLM body (with any unplaced frames
/// distributed inline by timestamp) + collapsed transcript.
///
/// Deterministic — does NOT call `Utc::now()`. The worker (Task 6) prepends the
/// `created` date field before writing.
pub fn build_note(raw: &RawMaterial, title: &str, llm_body: &str, frame_names: &[String]) -> String {
    let body = distribute_unplaced_frames(llm_body.trim(), raw, frame_names);

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("title: {title}\n"));
    out.push_str("tags: [видео, конспект]\n");
    out.push_str(&format!("duration: {:.0}s\n", raw.duration));
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(body.trim());
    out.push('\n');

    // Safety net: any frame still not present (e.g. empty body) goes in an
    // appendix so no frame is ever lost.
    let unplaced: Vec<&String> = frame_names.iter()
        .filter(|n| !body.contains(n.as_str()))
        .collect();
    if !unplaced.is_empty() {
        out.push_str("\n## Дополнительные кадры\n\n");
        for n in unplaced {
            out.push_str(&format!("![](images/{n})\n\n"));
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
    fn build_note_keeps_placed_frame_and_distributes_unplaced_inline() {
        let raw = RawMaterial {
            title: Some("Тест".into()), duration: 65.0, transcript: "речь целиком".into(),
            frames: vec![
                FrameDesc { timestamp: 5.0, description: "слайд".into(), image_b64: "x".into() },
                FrameDesc { timestamp: 9.0, description: "график".into(), image_b64: "y".into() },
            ],
            degraded: Degraded::default(),
        };
        let names = vec!["t-frame-01.jpg".to_string(), "t-frame-02.jpg".to_string()];
        // LLM placed frame 1 inline; frame 2 was left unplaced → distributed into body.
        let llm_body = "## Резюме\nкоротко\n\n## Конспект\n### Раздел\n![](images/t-frame-01.jpg)\n\nещё абзац";
        let note = build_note(&raw, "Тест", llm_body, &names);
        assert!(note.starts_with("---\n"), "frontmatter");
        assert!(note.contains("title: Тест"));
        assert!(note.contains("![](images/t-frame-01.jpg)"), "LLM-placed frame kept");
        assert!(note.contains("![](images/t-frame-02.jpg)"), "unplaced frame distributed");
        assert!(!note.contains("## Дополнительные кадры"), "no appendix — frame went inline");
        let f2 = note.find("t-frame-02.jpg").unwrap();
        let tr = note.find("Полный транскрипт").unwrap();
        assert!(f2 < tr, "distributed frame sits in the body, not after the transcript");
        assert!(note.contains("> [!note]- Полный транскрипт"));
        assert!(note.contains("речь целиком"));
    }

    #[test]
    fn build_note_distributes_all_frames_when_llm_embedded_none() {
        // Этап-2 failure mode: the LLM embedded ZERO frames. They must spread
        // through the body by timestamp, none dumped in a trailing appendix.
        let raw = RawMaterial {
            title: Some("Длинное".into()), duration: 120.0, transcript: "t".into(),
            frames: vec![
                FrameDesc { timestamp: 6.0, description: "a".into(), image_b64: "x".into() },
                FrameDesc { timestamp: 60.0, description: "b".into(), image_b64: "y".into() },
                FrameDesc { timestamp: 114.0, description: "c".into(), image_b64: "z".into() },
            ],
            degraded: Degraded::default(),
        };
        let names = vec!["f-01.jpg".to_string(), "f-02.jpg".to_string(), "f-03.jpg".to_string()];
        let llm_body = "## Конспект\n\n### Начало\nтекст1\n\n### Середина\nтекст2\n\n### Конец\nтекст3";
        let note = build_note(&raw, "Длинное", llm_body, &names);
        for n in &names {
            assert!(note.contains(&format!("![](images/{n})")), "{n} present");
        }
        assert!(!note.contains("## Дополнительные кадры"), "no appendix dump");
        // Spread by timestamp: early frame before late frame in the text.
        assert!(note.find("f-01.jpg").unwrap() < note.find("f-03.jpg").unwrap(),
            "early-timestamp frame placed before late-timestamp frame");
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
        assert!(user.content.contains("![](images/frame-01.jpg)"), "relative embed format used");
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
