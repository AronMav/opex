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

/// Write the frontmatter block (`---` … `---`) + `# title` heading into `out`.
fn push_frontmatter(out: &mut String, raw: &RawMaterial, title: &str) {
    out.push_str("---\n");
    out.push_str(&format!("title: {title}\n"));
    out.push_str("tags: [видео, конспект]\n");
    out.push_str(&format!("duration: {:.0}s\n", raw.duration));
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
}

/// Append the unplaced-frame appendix (frames whose embed string `body` omitted)
/// followed by the collapsed full transcript. Shared by `build_note` and
/// `build_note_from_parts`. `body` is the text that was already written so we
/// can detect which frame names it already references.
fn push_appendix_and_transcript(out: &mut String, raw: &RawMaterial, body: &str, frame_names: &[String]) {
    // Appendix: frames whose embed string the body did not include.
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
}

/// Build the full Obsidian note: frontmatter + LLM body + unplaced-frame appendix
/// + collapsed transcript.
///
/// Deterministic — does NOT call `Utc::now()`. The worker (Task 6) prepends the
/// `created` date field before writing.
pub fn build_note(raw: &RawMaterial, title: &str, llm_body: &str, frame_names: &[String]) -> String {
    let mut out = String::new();
    push_frontmatter(&mut out, raw, title);
    out.push_str(llm_body.trim());
    out.push('\n');
    push_appendix_and_transcript(&mut out, raw, llm_body, frame_names);
    out
}

// ── Map-reduce digest (topical segmentation) ─────────────────────────────────
//
// The single-pass digest feeds the whole transcript into ONE LLM call, which
// loses mid-transcript detail on long videos ("lost-in-the-middle"). Map-reduce
// instead:
//   1. one LLM call marks 5-8 topical segment boundaries (`segment_boundaries_messages`),
//   2. one LLM call per segment writes a DETAILED digest over only that slice
//      (small context → full retention) (`segment_digest_messages`),
//   3. the per-segment notes are concatenated IN ORDER (all detail preserved),
//   4. one final LLM call writes a short `## Резюме` over the merged body
//      (`final_summary_messages`),
//   5. `build_note_from_parts` assembles the deterministic note.
// Segments are NEVER rewritten in the reduce step — that would re-introduce the
// detail loss this mode exists to prevent.

const SEGMENT_BOUNDARIES_PROMPT: &str = "Ты разбиваешь транскрипт обучающего видео на \
ПОСЛЕДОВАТЕЛЬНЫЕ тематические сегменты для последующего конспектирования по частям.\n\
\n\
Раздели транскрипт на 5-8 сегментов по смыслу/темам. Сегменты идут ПО ПОРЯДКУ \
(не пересекаются, не переставляются) и вместе ПОКРЫВАЮТ ВЕСЬ транскрипт от начала до конца.\n\
\n\
Верни ТОЛЬКО JSON-массив, без пояснений, без markdown-обёртки, в формате:\n\
[{\"start_frac\": <число 0.0-1.0 — доля от начала транскрипта, где начинается сегмент>, \"title\": \"<краткая тема сегмента>\"}]\n\
\n\
Первый сегмент ОБЯЗАТЕЛЬНО начинается с start_frac 0.0. Доли возрастают строго по порядку. \
Сегментов 5-8. Только JSON-массив в ответе.";

const SEGMENT_DIGEST_PROMPT: &str = "Ты делаешь ПОДРОБНЫЙ русскоязычный конспект ОДНОГО \
тематического сегмента обучающего видео по фрагменту его транскрипта и описаниям ключевых кадров.\n\
\n\
Выведи ТОЛЬКО раздел(ы) конспекта этого сегмента в формате:\n\
### <тема сегмента> (<таймкод начала, напр. 3:20>)\n\
<развёрнутые пункты списком (-) или абзацы>\n\
\n\
НЕ пиши ## Резюме, НЕ пиши общий заголовок — только подробный конспект этого сегмента.\n\
\n\
КРИТИЧЕСКИ ВАЖНО — ПОДРОБНОСТЬ: раздел должен быть РАЗВЁРНУТЫМ — несколько пунктов или \
полноценные абзацы, а НЕ одна короткая строка. Изложи ВСЕ технические детали из фрагмента: \
точную последовательность действий, названия инструментов/плагинов/функций/кнопок, горячие \
клавиши, числовые значения и настройки (частоты, BPM, проценты, дБ), важные нюансы и причины \
(«зачем так делается»). Пиши настолько подробно, чтобы по конспекту можно было ПОВТОРИТЬ каждый \
шаг БЕЗ просмотра видео. НЕ опускай практические детали ради краткости.\n\
\n\
Тебе даны кадры этого сегмента с таймкодами и описаниями. После КАЖДОГО отдельного тезиса/пункта, \
к которому кадр относится по таймкоду и смыслу, вставь РОВНО ОДНУ embed-строку этого кадра. \
КАТЕГОРИЧЕСКИ НЕ группируй несколько кадров подряд. Размещай кадры ПО ОДНОМУ, разнося их по разным \
пунктам. Используй ВСЕ предоставленные кадры этого сегмента, каждый ровно один раз.\n\
\n\
Пиши по-русски, без воды.";

const FINAL_SUMMARY_PROMPT: &str = "Ты пишешь краткое резюме обучающего видео по уже готовому \
подробному конспекту.\n\
\n\
Прочитай конспект и выведи ТОЛЬКО краткое резюме: 3-5 предложений, передающих суть и главные темы \
видео. Без заголовков, без markdown-разметки, без списков — только связный текст резюме. \
Не пересказывай конспект целиком, только суть. Пиши по-русски.";

/// Helper: an LLM system+user message pair.
fn sys_user(system: &str, user: String) -> Vec<Message> {
    vec![
        Message {
            role: MessageRole::System,
            content: system.to_string(),
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

/// Map-reduce step 1: ask the LLM to mark 5-8 topical segment boundaries.
/// Returns a JSON array `[{"start_frac": f, "title": s}]` (parsing is the
/// worker's job — see `slice_segments`).
pub fn segment_boundaries_messages(raw: &RawMaterial) -> Vec<Message> {
    let mut user = String::new();
    user.push_str(&format!("Длительность видео: {:.0} сек.\n\n", raw.duration));
    user.push_str("=== Транскрипт ===\n");
    user.push_str(&raw.transcript);
    user.push_str("\n\nРазметь сегменты по инструкции. Верни только JSON-массив.");
    sys_user(SEGMENT_BOUNDARIES_PROMPT, user)
}

/// Map-reduce step 2: ask the LLM for a DETAILED digest of ONE segment.
/// Only this segment's transcript slice + its frames are passed (small context
/// → full retention). The model writes `### <тема> (таймкод)` sections, never a
/// `## Резюме`.
pub fn segment_digest_messages(
    raw: &RawMaterial,
    segment_transcript: &str,
    segment_frame_names: &[String],
) -> Vec<Message> {
    let mut user = String::new();
    user.push_str("=== Фрагмент транскрипта (этот сегмент) ===\n");
    user.push_str(segment_transcript);
    user.push_str("\n\n");

    if segment_frame_names.is_empty() {
        if raw.degraded.vision {
            user.push_str("(Описания кадров недоступны — vision-провайдер не активен; \
                           сделай конспект сегмента без кадров.)\n");
        }
    } else {
        user.push_str("=== Кадры этого сегмента (таймкод → описание → embed-строка) ===\n");
        // Match each requested frame_name back to its FrameDesc for the timecode/description.
        for name in segment_frame_names {
            if let Some((idx, _)) = frame_index_for_name(raw, name) {
                let f = &raw.frames[idx];
                user.push_str(&format!(
                    "[{:.0}s] {} → ![](images/{})\n",
                    f.timestamp, f.description, name
                ));
            } else {
                user.push_str(&format!("![](images/{name})\n"));
            }
        }
    }
    user.push_str("\nСделай подробный конспект этого сегмента по инструкции.");
    sys_user(SEGMENT_DIGEST_PROMPT, user)
}

/// Map-reduce step 4: ask the LLM for a short `## Резюме` over the merged body.
pub fn final_summary_messages(merged_body: &str) -> Vec<Message> {
    let mut user = String::new();
    user.push_str("=== Готовый конспект ===\n");
    user.push_str(merged_body);
    user.push_str("\n\nНапиши краткое резюме (3-5 предложений) по инструкции.");
    sys_user(FINAL_SUMMARY_PROMPT, user)
}

/// Recover the `frame-NN.jpg` → frame index mapping. `frame_names` are produced
/// in frame order as `frame-{:02}.jpg` (1-based), so index = NN-1; fall back to
/// a linear scan / `None` if the name is unexpected.
fn frame_index_for_name<'a>(raw: &'a RawMaterial, name: &str) -> Option<(usize, &'a FrameDesc)> {
    // Parse the zero-padded ordinal out of "frame-NN.jpg".
    let idx = name
        .strip_prefix("frame-")
        .and_then(|s| s.strip_suffix(".jpg"))
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.saturating_sub(1));
    if let Some(i) = idx
        && i < raw.frames.len()
    {
        return Some((i, &raw.frames[i]));
    }
    None
}

/// Slice the transcript into `(title, slice)` pairs by fractional boundaries.
///
/// `boundaries` is `(start_frac, title)` ascending in `start_frac` (0.0..1.0).
/// Each segment runs from its `start_frac` up to the next boundary's
/// `start_frac` (last runs to the end). Fractions are clamped to `[0,1]` and the
/// slice points are snapped to char boundaries so multibyte (Cyrillic) text is
/// never split mid-codepoint. The whole transcript is covered with no gaps.
pub fn slice_segments(transcript: &str, boundaries: &[(f64, String)]) -> Vec<(String, String)> {
    if boundaries.is_empty() {
        return vec![("Сегмент".to_string(), transcript.to_string())];
    }
    let len = transcript.len();
    // Snap a fractional position to a valid char boundary at or after the target byte.
    let snap = |frac: f64| -> usize {
        let target = (frac.clamp(0.0, 1.0) * len as f64).round() as usize;
        let target = target.min(len);
        let mut b = target;
        while b < len && !transcript.is_char_boundary(b) {
            b += 1;
        }
        b
    };
    let mut out = Vec::with_capacity(boundaries.len());
    for (i, (start_frac, title)) in boundaries.iter().enumerate() {
        let start = snap(*start_frac);
        let end = if i + 1 < boundaries.len() {
            snap(boundaries[i + 1].0)
        } else {
            len
        };
        // Guard against non-monotone fractions from a flaky LLM response.
        let (start, end) = if start <= end { (start, end) } else { (end, start) };
        out.push((title.clone(), transcript[start..end].to_string()));
    }
    out
}

/// Frames whose timestamp falls in this segment's fractional [start,end) range.
///
/// Frame position is `timestamp / duration`. The last segment is inclusive of
/// the upper bound so a frame exactly at `duration` is not dropped. Returns the
/// matching `frame_names` (aligned by index with `raw.frames`).
pub fn frames_for_segment(
    raw: &RawMaterial,
    frame_names: &[String],
    seg_start_frac: f64,
    seg_end_frac: f64,
) -> Vec<String> {
    let dur = if raw.duration > 0.0 { raw.duration } else { 1.0 };
    let lo = seg_start_frac.clamp(0.0, 1.0);
    let hi = seg_end_frac.clamp(0.0, 1.0);
    let is_last = hi >= 1.0;
    raw.frames
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let frac = (f.timestamp / dur).clamp(0.0, 1.0);
            let in_range = frac >= lo && (frac < hi || (is_last && frac <= hi));
            if in_range {
                frame_names.get(i).cloned()
            } else {
                None
            }
        })
        .collect()
}

/// Assemble the full Obsidian note from already-produced map-reduce parts:
/// frontmatter + `## Резюме` + `## Конспект` (merged segment bodies) +
/// unplaced-frame appendix + collapsed transcript. Deterministic.
pub fn build_note_from_parts(
    raw: &RawMaterial,
    title: &str,
    summary: &str,
    merged_body: &str,
    frame_names: &[String],
) -> String {
    let mut out = String::new();
    push_frontmatter(&mut out, raw, title);
    out.push_str("## Резюме\n\n");
    out.push_str(summary.trim());
    out.push_str("\n\n## Конспект\n\n");
    out.push_str(merged_body.trim());
    out.push('\n');
    // Appendix detects placed frames from the merged segment bodies.
    push_appendix_and_transcript(&mut out, raw, merged_body, frame_names);
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
        let llm_body = "## Резюме\nкоротко\n\n## Конспект\n### Раздел\n![](images/t-frame-01.jpg)\n";
        let note = build_note(&raw, "Тест", llm_body, &names);
        assert!(note.starts_with("---\n"), "frontmatter");
        assert!(note.contains("title: Тест"));
        assert!(note.contains("![](images/t-frame-01.jpg)"));
        assert!(note.contains("## Дополнительные кадры"));
        assert!(note.contains("![](images/t-frame-02.jpg)"), "unplaced frame appended");
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

    // ── Map-reduce helpers ────────────────────────────────────────────────────

    fn mr_raw() -> RawMaterial {
        RawMaterial {
            title: Some("MR".into()),
            duration: 100.0,
            // 40 chars of ASCII for predictable fractional slicing.
            transcript: "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ0123".into(),
            frames: vec![
                FrameDesc { timestamp: 10.0, description: "k1".into(), image_b64: "a".into() },
                FrameDesc { timestamp: 50.0, description: "k2".into(), image_b64: "b".into() },
                FrameDesc { timestamp: 95.0, description: "k3".into(), image_b64: "c".into() },
            ],
            degraded: Degraded::default(),
        }
    }

    #[test]
    fn slice_segments_covers_transcript_in_order() {
        let raw = mr_raw(); // transcript len = 40
        let bounds = vec![
            (0.0, "A".to_string()),
            (0.25, "B".to_string()),
            (0.75, "C".to_string()),
        ];
        let segs = slice_segments(&raw.transcript, &bounds);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].0, "A");
        // Full coverage: concatenating slices reconstructs the transcript exactly.
        let joined: String = segs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, raw.transcript, "slices cover the whole transcript with no gaps/overlap");
        // 0.0..0.25 of 40 = [0,10); 0.25..0.75 = [10,30); 0.75..1.0 = [30,40)
        assert_eq!(segs[0].1, "0123456789");
        assert_eq!(segs[1].1, "ABCDEFGHIJKLMNOPQRST");
        assert_eq!(segs[2].1, "UVWXYZ0123");
    }

    #[test]
    fn slice_segments_empty_boundaries_returns_whole() {
        let segs = slice_segments("весь текст", &[]);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].1, "весь текст");
    }

    #[test]
    fn slice_segments_respects_char_boundaries() {
        // Cyrillic = 2 bytes/char. A frac landing mid-char must snap forward,
        // never panic, and still fully cover the input.
        let t = "абвгдеёжзи"; // 10 chars, 20 bytes
        let bounds = vec![(0.0, "x".to_string()), (0.35, "y".to_string())];
        let segs = slice_segments(t, &bounds);
        let joined: String = segs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(joined, t, "multibyte slices still cover whole input");
    }

    #[test]
    fn frames_for_segment_partitions_by_fraction() {
        let raw = mr_raw(); // frames at 10s, 50s, 95s of 100s → fracs 0.10, 0.50, 0.95
        let names = vec!["frame-01.jpg".to_string(), "frame-02.jpg".to_string(), "frame-03.jpg".to_string()];
        // Segment 0: [0.0, 0.25) → frame 0 (0.10)
        let s0 = frames_for_segment(&raw, &names, 0.0, 0.25);
        assert_eq!(s0, vec!["frame-01.jpg".to_string()]);
        // Segment 1: [0.25, 0.75) → frame 1 (0.50)
        let s1 = frames_for_segment(&raw, &names, 0.25, 0.75);
        assert_eq!(s1, vec!["frame-02.jpg".to_string()]);
        // Segment 2 (last): [0.75, 1.0] inclusive → frame 2 (0.95)
        let s2 = frames_for_segment(&raw, &names, 0.75, 1.0);
        assert_eq!(s2, vec!["frame-03.jpg".to_string()]);
    }

    #[test]
    fn frames_for_segment_zero_duration_safe() {
        let mut raw = mr_raw();
        raw.duration = 0.0;
        let names = vec!["frame-01.jpg".to_string(), "frame-02.jpg".to_string(), "frame-03.jpg".to_string()];
        // No division-by-zero panic. With unknown duration we use dur=1.0, so any
        // nonzero timestamp clamps to frac 1.0 → all frames land in the LAST segment.
        let s0 = frames_for_segment(&raw, &names, 0.0, 0.5);
        assert!(s0.is_empty(), "non-last segment gets no frames when duration is 0");
        let s_last = frames_for_segment(&raw, &names, 0.5, 1.0);
        assert_eq!(s_last.len(), 3, "all frames land in the last segment when duration is 0");
    }

    #[test]
    fn build_note_from_parts_has_summary_digest_and_transcript() {
        let raw = mr_raw();
        let names = vec!["frame-01.jpg".to_string(), "frame-02.jpg".to_string()];
        let summary = "Краткое резюме видео.";
        // Merged body references only frame 1; frame 2 must land in the appendix.
        let merged = "### Тема 1 (0:10)\n- пункт\n![](images/frame-01.jpg)\n\n### Тема 2 (0:50)\n- ещё пункт";
        let note = build_note_from_parts(&raw, "MR", summary, merged, &names);
        assert!(note.starts_with("---\n"), "frontmatter first");
        assert!(note.contains("title: MR"));
        assert!(note.contains("# MR\n"));
        assert!(note.contains("## Резюме\n\nКраткое резюме видео."), "summary section");
        assert!(note.contains("## Конспект\n\n### Тема 1"), "digest section");
        assert!(note.contains("![](images/frame-01.jpg)"), "placed frame retained");
        assert!(note.contains("## Дополнительные кадры"));
        assert!(note.contains("![](images/frame-02.jpg)"), "unplaced frame appended");
        assert!(note.contains("> [!note]- Полный транскрипт"));
        assert!(note.contains(&raw.transcript), "full transcript collapsed in");
        // extract_summary must read the generated ## Резюме section.
        assert_eq!(extract_summary(&note).trim(), "Краткое резюме видео.");
    }

    #[test]
    fn segment_boundaries_prompt_smoke() {
        let raw = mr_raw();
        let msgs = segment_boundaries_messages(&raw);
        assert_eq!(msgs[0].role, MessageRole::System);
        let user = &msgs[msgs.len() - 1];
        assert_eq!(user.role, MessageRole::User);
        assert!(user.content.contains(&raw.transcript), "transcript embedded");
        assert!(msgs[0].content.contains("JSON"), "asks for JSON array");
        assert!(msgs[0].content.contains("start_frac"), "uses fractional boundaries");
    }

    #[test]
    fn segment_digest_prompt_smoke() {
        let raw = mr_raw();
        let names = vec!["frame-02.jpg".to_string()];
        let msgs = segment_digest_messages(&raw, "фрагмент текста сегмента", &names);
        assert_eq!(msgs[0].role, MessageRole::System);
        let user = &msgs[msgs.len() - 1];
        assert_eq!(user.role, MessageRole::User);
        assert!(user.content.contains("фрагмент текста сегмента"), "segment slice embedded");
        assert!(user.content.contains("![](images/frame-02.jpg)"), "segment frame embedded");
        assert!(user.content.contains("k2"), "matched frame description embedded");
        assert!(msgs[0].content.contains("НЕ пиши ## Резюме"), "segment prompt forbids summary");
    }

    #[test]
    fn final_summary_prompt_smoke() {
        let merged = "### Тема\n- много деталей";
        let msgs = final_summary_messages(merged);
        assert_eq!(msgs[0].role, MessageRole::System);
        let user = &msgs[msgs.len() - 1];
        assert_eq!(user.role, MessageRole::User);
        assert!(user.content.contains(merged), "merged body embedded");
        assert!(msgs[0].content.contains("резюме"), "asks for a summary");
    }
}
