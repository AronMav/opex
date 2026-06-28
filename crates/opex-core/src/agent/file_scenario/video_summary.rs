//! Builds the Obsidian note and LLM digest prompt from toolgate raw material.
//! The whole transcript goes in (large context — no telesumbot 40k chunking).
//! Prompts ported from telesumbot `summary/prompts.rs`.

use serde::Deserialize;
use opex_types::{Message, MessageRole};

#[derive(Debug, Clone, Deserialize)]
pub struct FrameDesc {
    /// Absolute seconds of the frame. Used to assign a frame's on-screen
    /// description to its time-chunk in the chunked (long-video) digest.
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

// ── Chunked digest (long videos) ───────────────────────────────────────────────
//
// Single-pass digest is great up to ~2 h, but a multi-hour transcript gets
// diluted (lost-in-the-middle + the model's modest output budget) — a 6 h note
// ended up sparser per hour than a 2 h one. For long videos we map-reduce:
// split the transcript into time windows, write a DETAILED partial conspect for
// each window (small context → full density), then merge the partials into one
// coherent note. The merge is told to PRESERVE detail (only dedupe/restructure),
// not re-summarise — otherwise the reduce step would re-introduce the dilution.

/// Videos longer than this (whole minutes of transcript) use the chunked
/// map-reduce digest; shorter ones keep the single-pass path.
pub const DIGEST_CHUNK_THRESHOLD_MIN: u32 = 150;

/// Length of each transcript window (minutes) in the chunked digest.
pub const DIGEST_CHUNK_MINUTES: u32 = 45;

/// Parse the leading `[MM:SS]` / `[MMM:SS]` marker of a transcript line into
/// whole minutes. Returns None for lines without a valid leading timecode.
fn parse_line_minute(line: &str) -> Option<u32> {
    let rest = line.trim_start().strip_prefix('[')?;
    let close = rest.find(']')?;
    let (m, s) = rest[..close].split_once(':')?;
    if m.is_empty() || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    m.parse::<u32>().ok()
}

/// Whole minutes covered by the transcript (its last `[MM:SS]` marker; 0 if none).
pub fn transcript_minutes(transcript: &str) -> u32 {
    transcript.lines().rev().find_map(parse_line_minute).unwrap_or(0)
}

/// True when the transcript is long enough to warrant the chunked map-reduce digest.
pub fn should_chunk(transcript: &str) -> bool {
    transcript_minutes(transcript) > DIGEST_CHUNK_THRESHOLD_MIN
}

/// One time-window of the transcript (raw lines, timecodes intact — they are
/// stripped at prompt-build time).
#[derive(Debug, Clone)]
pub struct TranscriptChunk {
    pub start_min: u32,
    pub end_min: u32,
    pub text: String,
}

/// Split the timecoded transcript into `chunk_min`-minute windows on line
/// boundaries (each line is one STT segment). A line's window is `minute /
/// chunk_min`; lines without a timecode inherit the previous line's minute, so
/// any preamble before the first marker lands in window 0.
pub fn split_transcript_by_time(transcript: &str, chunk_min: u32) -> Vec<TranscriptChunk> {
    let chunk_min = chunk_min.max(1);
    let mut chunks: Vec<TranscriptChunk> = Vec::new();
    let mut cur_idx: Option<u32> = None;
    let mut cur_lines: Vec<&str> = Vec::new();
    let mut last_min: u32 = 0;
    for line in transcript.lines() {
        let min = parse_line_minute(line).unwrap_or(last_min);
        last_min = min;
        let idx = min / chunk_min;
        match cur_idx {
            None => cur_idx = Some(idx),
            Some(c) if idx != c => {
                chunks.push(TranscriptChunk {
                    start_min: c * chunk_min,
                    end_min: (c + 1) * chunk_min,
                    text: std::mem::take(&mut cur_lines).join("\n"),
                });
                cur_idx = Some(idx);
            }
            _ => {}
        }
        cur_lines.push(line);
    }
    if let Some(c) = cur_idx {
        chunks.push(TranscriptChunk {
            start_min: c * chunk_min,
            end_min: last_min + 1,
            text: cur_lines.join("\n"),
        });
    }
    chunks
}

fn sys_msg(content: &str) -> Message {
    Message {
        role: MessageRole::System,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
        db_id: None,
    }
}

fn user_msg(content: String) -> Message {
    Message {
        role: MessageRole::User,
        content,
        tool_calls: None,
        tool_call_id: None,
        thinking_blocks: vec![],
        db_id: None,
    }
}

const CHUNK_SYSTEM_PROMPT: &str = "Ты делаешь ПОДРОБНЫЙ конспект ОДНОГО фрагмента длинной \
видео-лекции. Перед тобой транскрипт ТОЛЬКО этого фрагмента. Сделай развёрнутый \
структурированный конспект именно этого фрагмента с подзаголовками ### и пунктами списком.\n\
\n\
Изложи ВСЕ технические детали из фрагмента: точную последовательность действий, названия \
инструментов/функций/настроек/кнопок, горячие клавиши, числовые значения и параметры, формулы, \
команды, важные нюансы и причины («зачем так делается»). Пиши настолько подробно, чтобы по \
конспекту можно было ПОВТОРИТЬ материал без просмотра. НЕ опускай детали ради краткости.\n\
\n\
НЕ добавляй таймкоды. НЕ добавляй раздел «Резюме» и общие выводы про всю лекцию — только \
содержательный конспект ЭТОГО фрагмента. НЕ вставляй изображения или embed-строки вида ![](...). \
По-русски, без воды.";

/// Build the map-step messages: a detailed partial conspect for one time-window.
/// Frame descriptions whose timestamp lands inside the window are passed as
/// on-screen context. `idx` is 0-based; `total` is the chunk count.
pub fn build_chunk_messages(
    chunk: &TranscriptChunk,
    idx: usize,
    total: usize,
    frames: &[FrameDesc],
) -> Vec<Message> {
    let mut user = String::new();
    user.push_str(&format!(
        "Фрагмент {} из {} (примерно минуты {}–{}).\n\n",
        idx + 1,
        total,
        chunk.start_min,
        chunk.end_min
    ));
    user.push_str("=== Транскрипт фрагмента ===\n");
    user.push_str(&strip_transcript_timecodes(&chunk.text));
    user.push_str("\n\n");

    let lo = chunk.start_min as f64 * 60.0;
    let hi = chunk.end_min as f64 * 60.0;
    let mut any_frame = false;
    for f in frames {
        if f.timestamp >= lo && f.timestamp < hi && !f.description.trim().is_empty() {
            if !any_frame {
                user.push_str("=== Что показано на экране в этом фрагменте ===\n");
                any_frame = true;
            }
            user.push_str(&format!("- {}\n", f.description));
        }
    }
    user.push_str("\nСделай подробный конспект этого фрагмента.");

    vec![sys_msg(CHUNK_SYSTEM_PROMPT), user_msg(user)]
}

const REDUCE_SYSTEM_PROMPT: &str = "Тебе даны подробные конспекты последовательных фрагментов \
ОДНОЙ видео-лекции, по порядку. Объедини их в ОДИН цельный конспект.\n\
\n\
Выведи ДВА раздела, точно в таком формате (ничего лишнего до первого раздела):\n\
\n\
## Резюме\n\
<3-5 предложений — суть всей лекции>\n\
\n\
## Конспект\n\
<подробный конспект всей лекции с подзаголовками ###, сгруппированный по темам>\n\
\n\
КРИТИЧЕСКИ ВАЖНО: СОХРАНИ ВСЕ детали, формулы, числа, названия, команды и нюансы из фрагментов — \
ничего не выбрасывай и НЕ сокращай ради краткости. Твоя задача — ОБЪЕДИНИТЬ и упорядочить материал \
по темам и убрать только дословные повторы на стыках фрагментов, а НЕ пересказать короче. Итоговый \
конспект должен быть по объёму и детальности сопоставим с суммой фрагментов, а не короче их.\n\
\n\
НЕ добавляй таймкоды. НЕ вставляй изображения или embed-строки. По-русски.";

/// Build the reduce-step messages: merge the per-chunk partial conspects into one
/// coherent note (## Резюме + ## Конспект), preserving all detail.
pub fn build_reduce_messages(partials: &[String]) -> Vec<Message> {
    let total = partials.len();
    let mut user = String::new();
    user.push_str(&format!(
        "Ниже {total} конспектов последовательных фрагментов лекции (по порядку времени). \
         Объедини их в один конспект по инструкции.\n\n"
    ));
    for (i, p) in partials.iter().enumerate() {
        user.push_str(&format!("===== Фрагмент {}/{} =====\n", i + 1, total));
        user.push_str(p.trim());
        user.push_str("\n\n");
    }
    user.push_str("Склей фрагменты в единый конспект, сохранив все детали.");

    vec![sys_msg(REDUCE_SYSTEM_PROMPT), user_msg(user)]
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

    // ── Chunked digest ───────────────────────────────────────────────────────

    #[test]
    fn transcript_minutes_and_should_chunk_threshold() {
        let short = "[00:04] раз\n[120:00] два"; // 2h
        assert_eq!(transcript_minutes(short), 120);
        assert!(!should_chunk(short), "2h stays single-pass");
        let long = "[00:00] a\n[200:30] b"; // 3h20m
        assert_eq!(transcript_minutes(long), 200);
        assert!(should_chunk(long), ">2.5h uses chunked");
        assert_eq!(transcript_minutes("без меток"), 0);
    }

    #[test]
    fn split_transcript_by_time_groups_lines_into_windows() {
        // 45-min windows: lines at 0,10,44 -> window 0; 46,89 -> window 1; 91 -> window 2.
        let t = "[00:10] a\n[10:00] b\n[44:59] c\nпродолжение без метки\n[46:00] d\n[89:00] e\n[91:00] f";
        let chunks = split_transcript_by_time(t, 45);
        assert_eq!(chunks.len(), 3, "three 45-min windows");
        assert_eq!((chunks[0].start_min, chunks[0].end_min), (0, 45));
        assert!(chunks[0].text.contains("[00:10] a") && chunks[0].text.contains("[44:59] c"));
        assert!(chunks[0].text.contains("продолжение без метки"), "untimed line attaches to current window");
        assert_eq!(chunks[1].start_min, 45);
        assert!(chunks[1].text.contains("[46:00] d") && chunks[1].text.contains("[89:00] e"));
        assert!(chunks[2].text.contains("[91:00] f"));
    }

    #[test]
    fn build_chunk_messages_strips_timecodes_labels_part_and_filters_frames() {
        let chunk = TranscriptChunk { start_min: 45, end_min: 90, text: "[46:00] середина урока".into() };
        let frames = vec![
            FrameDesc { timestamp: 30.0, description: "кадр из части 1".into(), image_b64: String::new() },
            FrameDesc { timestamp: 3000.0, description: "кадр из части 2".into(), image_b64: String::new() }, // 50 min
        ];
        let msgs = build_chunk_messages(&chunk, 1, 5, &frames);
        assert_eq!(msgs[0].role, MessageRole::System);
        let user = &msgs[1].content;
        assert!(user.contains("Фрагмент 2 из 5"), "1-based part label");
        assert!(user.contains("минуты 45–90"));
        assert!(user.contains("середина урока") && !user.contains("[46:00]"), "timecodes stripped");
        assert!(user.contains("кадр из части 2"), "in-window frame included");
        assert!(!user.contains("кадр из части 1"), "out-of-window frame excluded");
    }

    #[test]
    fn build_reduce_messages_includes_all_partials_in_order() {
        let partials = vec!["конспект A".to_string(), "конспект B".to_string()];
        let msgs = build_reduce_messages(&partials);
        assert!(msgs[0].content.contains("## Резюме") && msgs[0].content.contains("СОХРАНИ ВСЕ"),
            "reduce system demands format + detail preservation");
        let user = &msgs[1].content;
        let a = user.find("конспект A").unwrap();
        let b = user.find("конспект B").unwrap();
        assert!(a < b, "partials kept in order");
        assert!(user.contains("Фрагмент 1/2") && user.contains("Фрагмент 2/2"));
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
