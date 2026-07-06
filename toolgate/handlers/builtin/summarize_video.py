# <handler>
#   <id>summarize_video</id>
#   <label lang="ru">Конспект видео</label>
#   <label lang="en">Summarize Video</label>
#   <description lang="ru">Транскрибирует видео и создаёт структурированный конспект</description>
#   <description lang="en">Transcribes video and creates a structured summary</description>
#   <icon>video</icon>
#   <match>
#     <mime>video/*</mime>
#     <max_size_mb>2000</max_size_mb>
#     <domain>youtube.com</domain>
#     <domain>youtu.be</domain>
#     <domain>yadi.sk</domain>
#     <domain>disk.yandex.ru</domain>
#     <domain>disk.yandex.com</domain>
#     <domain>disk.yandex.kz</domain>
#     <domain>disk.yandex.by</domain>
#     <domain>disk.yandex.uz</domain>
#   </match>
#   <capability>stt</capability>
#   <execution>async</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>20</order>
#   <enabled>true</enabled>
# </handler>
"""summarize_video — full LLM map-reduce digest handler (Phase 5, Task 6c).

Ports the digest logic from `crates/opex-core/src/agent/file_scenario/video_summary.rs`
into Python. Uses ctx.llm.complete (Task 6b) for LLM calls and ctx.stt.transcribe for
speech-to-text. Returns an obsidian_note post_action for the runner to write to vault.

NOTE: Frame/vision descriptions in the digest prompts are DEFERRED — the Rust code
passes frame descriptions as on-screen context to enrich the LLM prompt. This handler
does transcript-only digests for now; vision enrichment is a follow-up task.

R12: bytes arrive via file.bytes (no loopback fetch). The `extract_audio_from_file`
seam allows tests to stub audio extraction without ffmpeg.
"""
from __future__ import annotations

import asyncio
import hashlib
import os
import re
import tempfile
from typing import Optional

# ── constants (ported from video_summary.rs) ──────────────────────────────────

DIGEST_CHUNK_THRESHOLD_MIN: int = 150
"""Use map-reduce only when transcript_minutes STRICTLY > this value."""

DIGEST_CHUNK_MINUTES: int = 45
"""Width of each transcript time-window (minutes) in the chunked digest."""

# ── Cyrillic → Latin transliteration table ────────────────────────────────────
# Only the most common Cyrillic letters; unmapped chars are dropped by latin_slug.
_TRANSLIT: dict[str, str] = {
    "а": "a", "б": "b", "в": "v", "г": "g", "д": "d", "е": "e", "ё": "yo",
    "ж": "zh", "з": "z", "и": "i", "й": "y", "к": "k", "л": "l", "м": "m",
    "н": "n", "о": "o", "п": "p", "р": "r", "с": "s", "т": "t", "у": "u",
    "ф": "f", "х": "kh", "ц": "ts", "ч": "ch", "ш": "sh", "щ": "sch",
    "ъ": "", "ы": "y", "ь": "", "э": "e", "ю": "yu", "я": "ya",
    "А": "A", "Б": "B", "В": "V", "Г": "G", "Д": "D", "Е": "E", "Ё": "Yo",
    "Ж": "Zh", "З": "Z", "И": "I", "Й": "Y", "К": "K", "Л": "L", "М": "M",
    "Н": "N", "О": "O", "П": "P", "Р": "R", "С": "S", "Т": "T", "У": "U",
    "Ф": "F", "Х": "Kh", "Ц": "Ts", "Ч": "Ch", "Ш": "Sh", "Щ": "Sch",
    "Ъ": "", "Ы": "Y", "Ь": "", "Э": "E", "Ю": "Yu", "Я": "Ya",
}

# Core's path guard: ^[A-Za-z0-9 _.-]{1,128}$
_LATIN_SAFE_RE = re.compile(r"[^A-Za-z0-9 _.\-]")


def latin_slug(title: str, fallback_id: str) -> str:
    """Build a Latin-safe slug from `title`.

    Transliterates Cyrillic to Latin, strips remaining non-ASCII chars,
    collapses whitespace, and trims to 100 chars. Falls back to
    `video-{fallback_id}` if the result is empty.

    Core's `run_post_action` path guard is `^[A-Za-z0-9 _.-]{1,128}$` —
    Cyrillic is rejected, hence the transliteration (vs. the Rust `slug`
    which keeps Cyrillic for the Obsidian-only context).
    """
    # Transliterate Cyrillic
    out_chars: list[str] = []
    for ch in title:
        if ch in _TRANSLIT:
            out_chars.append(_TRANSLIT[ch])
        elif ch in "/:*?\"<>|#\\":
            out_chars.append(" ")
        else:
            out_chars.append(ch)
    transliterated = "".join(out_chars)
    # Strip any remaining non-Latin-safe chars
    cleaned = _LATIN_SAFE_RE.sub(" ", transliterated)
    # Collapse whitespace and join with hyphens
    parts = cleaned.split()
    slug = "-".join(parts)
    # Trim
    slug = slug[:100].strip("-")
    if not slug:
        short = fallback_id[:8] if fallback_id else "unknown"
        slug = f"video-{short}"
    return slug


# ── prompts (ported from video_summary.rs) ────────────────────────────────────

SYSTEM_PROMPT: str = (
    "Ты помощник, который делает структурированный русскоязычный "
    "конспект видео по его транскрипту и описаниям ключевых кадров. "
    "Выведи ДВА раздела, точно в таком формате (ничего лишнего до первого раздела):\n"
    "\n"
    "## Резюме\n"
    "<3-5 предложений, суть видео>\n"
    "\n"
    "## Конспект\n"
    "<ПОДРОБНЫЙ пошаговый конспект с подзаголовками ###>\n"
    "\n"
    "НЕ добавляй таймкоды или тайминги (например «[00:00]», «5:30», «(2:15)») нигде в конспекте — "
    "ни в заголовки ###, ни в пункты. Конспект должен быть чистым связным текстом без таймингов.\n"
    "\n"
    "КРИТИЧЕСКИ ВАЖНО — ПОДРОБНОСТЬ: каждый раздел ## Конспекта должен быть РАЗВЁРНУТЫМ — несколько "
    "пунктов списком (-) или полноценный абзац, а НЕ одна короткая строка-аннотация. В каждом разделе "
    "изложи ВСЕ технические детали из транскрипта: точную последовательность действий, названия "
    "инструментов/плагинов/функций/кнопок, горячие клавиши, числовые значения и настройки (частоты, BPM, "
    "проценты, дБ), важные нюансы и причины («зачем так делается»). Пиши настолько подробно, чтобы по "
    "конспекту можно было ПОВТОРИТЬ каждый шаг урока БЕЗ просмотра видео. НЕ опускай практические детали, "
    "приёмы и второстепенные советы ради краткости — лучше длиннее и полнее, чем коротко.\n"
    "\n"
    "Тебе также даны описания того, ЧТО ПОКАЗАНО НА ЭКРАНЕ в ключевые моменты (окна плагинов, "
    "панели настроек, значения параметров). Используй их, чтобы точнее и подробнее изложить детали "
    "в ТЕКСТЕ конспекта (точные значения, названия окон/параметров, что именно видно). "
    "НЕ вставляй в конспект изображения, кадры или embed-строки вида ![](...) — только текст.\n"
    "\n"
    "Пиши по-русски, без воды."
)

CHUNK_SYSTEM_PROMPT: str = (
    "Ты делаешь ПОДРОБНЫЙ конспект ОДНОГО фрагмента длинной "
    "видео-лекции. Перед тобой транскрипт ТОЛЬКО этого фрагмента. Сделай развёрнутый "
    "структурированный конспект именно этого фрагмента с подзаголовками ### и пунктами списком.\n"
    "\n"
    "Изложи ВСЕ технические детали из фрагмента: точную последовательность действий, названия "
    "инструментов/функций/настроек/кнопок, горячие клавиши, числовые значения и параметры, формулы, "
    "команды, важные нюансы и причины («зачем так делается»). Пиши настолько подробно, чтобы по "
    "конспекту можно было ПОВТОРИТЬ материал без просмотра. НЕ опускай детали ради краткости.\n"
    "\n"
    "НЕ добавляй таймкоды. НЕ добавляй раздел «Резюме» и общие выводы про всю лекцию — только "
    "содержательный конспект ЭТОГО фрагмента. НЕ вставляй изображения или embed-строки вида ![](...). "
    "По-русски, без воды."
)

REDUCE_SYSTEM_PROMPT: str = (
    "Тебе даны подробные конспекты последовательных фрагментов "
    "ОДНОЙ видео-лекции, по порядку. Объедини их в ОДИН цельный конспект.\n"
    "\n"
    "Выведи ДВА раздела, точно в таком формате (ничего лишнего до первого раздела):\n"
    "\n"
    "## Резюме\n"
    "<3-5 предложений — суть всей лекции>\n"
    "\n"
    "## Конспект\n"
    "<подробный конспект всей лекции с подзаголовками ###, сгруппированный по темам>\n"
    "\n"
    "КРИТИЧЕСКИ ВАЖНО: СОХРАНИ ВСЕ детали, формулы, числа, названия, команды и нюансы из фрагментов — "
    "ничего не выбрасывай и НЕ сокращай ради краткости. Твоя задача — ОБЪЕДИНИТЬ и упорядочить материал "
    "по темам и убрать только дословные повторы на стыках фрагментов, а НЕ пересказать короче. Итоговый "
    "конспект должен быть по объёму и детальности сопоставим с суммой фрагментов, а не короче их.\n"
    "\n"
    "ЕДИНЫЙ ДОКУМЕНТ ДЛЯ ЧИТАТЕЛЯ — конспект ОДНОЙ лекции, механика обработки не должна быть видна:\n"
    "- НЕ упоминай в тексте и заголовках «фрагменты», «части», «по порядку», «из разных фрагментов» и "
    "т.п. — для читателя это цельный конспект.\n"
    "- НЕ создавай два раздела с похожими или дублирующими заголовками — объединяй такой материал в один "
    "раздел.\n"
    "- Материал из вопросов-ответов встраивай в соответствующие тематические разделы; то, что не "
    "привязывается к теме, собери в ОДИН раздел «Вопросы и ответы» в конце, а не разбрасывай по "
    "нескольким разделам.\n"
    "\n"
    "НЕ добавляй таймкоды. НЕ вставляй изображения или embed-строки. По-русски."
)


# ── data types ────────────────────────────────────────────────────────────────

class TranscriptChunk:
    """One time-window of the transcript (raw lines, timecodes intact)."""

    __slots__ = ("start_min", "end_min", "text")

    def __init__(self, start_min: int, end_min: int, text: str) -> None:
        self.start_min = start_min
        self.end_min = end_min
        self.text = text


# ── transcript helpers ────────────────────────────────────────────────────────

def _parse_line_minute(line: str) -> Optional[int]:
    """Parse `[MM:SS]` or `[MMM:SS]` prefix → whole minutes. None if absent."""
    stripped = line.lstrip()
    if not stripped.startswith("["):
        return None
    close = stripped.find("]")
    if close < 0:
        return None
    inner = stripped[1:close]
    if ":" not in inner:
        return None
    m_str, s_str = inner.split(":", 1)
    if not m_str or not m_str.isdigit():
        return None
    if len(s_str) != 2 or not s_str.isdigit():
        return None
    return int(m_str)


def transcript_minutes(transcript: str) -> int:
    """Whole minutes of the last `[MM:SS]` timecode in `transcript` (0 if none)."""
    for line in reversed(transcript.splitlines()):
        m = _parse_line_minute(line)
        if m is not None:
            return m
    return 0


def should_chunk(transcript: str) -> bool:
    """True when transcript is STRICTLY longer than DIGEST_CHUNK_THRESHOLD_MIN minutes."""
    return transcript_minutes(transcript) > DIGEST_CHUNK_THRESHOLD_MIN


def split_transcript_by_time(
    transcript: str, chunk_min: int = DIGEST_CHUNK_MINUTES
) -> list[TranscriptChunk]:
    """Split timecoded transcript into `chunk_min`-minute windows.

    Lines without a timecode inherit the previous line's minute (or 0 for
    leading lines before the first marker).
    """
    chunk_min = max(1, chunk_min)
    chunks: list[TranscriptChunk] = []
    cur_idx: Optional[int] = None
    cur_lines: list[str] = []
    last_min: int = 0

    for line in transcript.splitlines():
        m = _parse_line_minute(line)
        if m is not None:
            last_min = m
        minute = last_min
        idx = minute // chunk_min

        if cur_idx is None:
            cur_idx = idx
        elif idx != cur_idx:
            chunks.append(TranscriptChunk(
                start_min=cur_idx * chunk_min,
                end_min=(cur_idx + 1) * chunk_min,
                text="\n".join(cur_lines),
            ))
            cur_idx = idx
            cur_lines = []

        cur_lines.append(line)

    if cur_idx is not None:
        chunks.append(TranscriptChunk(
            start_min=cur_idx * chunk_min,
            end_min=last_min + 1,
            text="\n".join(cur_lines),
        ))

    return chunks


def strip_transcript_timecodes(text: str) -> str:
    """Strip leading `[MM:SS]` markers so the LLM sees clean text (no timecodes)."""
    result: list[str] = []
    for line in text.splitlines():
        stripped = line.lstrip()
        m = _parse_line_minute(line)
        if m is not None:
            # Find the closing ] and take everything after it
            close = stripped.find("]")
            after = stripped[close + 1:].lstrip()
            result.append(after)
        else:
            result.append(line)
    return "\n".join(result)


def _strip_image_embeds(body: str) -> str:
    """Remove `![](images/...)` lines (screenshots removed from notes)."""
    return "\n".join(
        line for line in body.splitlines()
        if not (line.strip().startswith("![](images/") and line.strip().endswith(")"))
    )


# ── prompt builders ───────────────────────────────────────────────────────────

def build_single_pass_messages(
    transcript: str,
    duration: float = 0.0,
) -> list[dict]:
    """Messages for the single-pass digest (short transcripts).

    NOTE: Frame descriptions are DEFERRED — the Rust version passes vision
    frame descriptions here. This implementation is transcript-only.
    """
    user_parts = []
    if duration > 0:
        user_parts.append(f"Длительность видео: {duration:.0f} сек.\n")
    user_parts.append("=== Транскрипт ===\n")
    user_parts.append(strip_transcript_timecodes(transcript))
    user_parts.append("\n\nСделай конспект по инструкции.")
    return [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": "\n".join(user_parts)},
    ]


def build_chunk_messages(
    chunk: TranscriptChunk,
    idx: int,
    total: int,
) -> list[dict]:
    """Messages for one map-step partial conspect.

    NOTE: Frame descriptions filtered to this chunk's time window are DEFERRED.
    """
    user = (
        f"Фрагмент {idx + 1} из {total} "
        f"(примерно минуты {chunk.start_min}–{chunk.end_min}).\n\n"
        "=== Транскрипт фрагмента ===\n"
        f"{strip_transcript_timecodes(chunk.text)}\n\n"
        "Сделай подробный конспект этого фрагмента."
    )
    return [
        {"role": "system", "content": CHUNK_SYSTEM_PROMPT},
        {"role": "user", "content": user},
    ]


def build_reduce_messages(partials: list[str]) -> list[dict]:
    """Messages for the reduce step: merge per-chunk partial conspects."""
    total = len(partials)
    user_parts = [
        f"Ниже {total} конспектов последовательных фрагментов лекции (по порядку времени). "
        "Объедини их в один конспект по инструкции.\n\n"
    ]
    for i, p in enumerate(partials):
        user_parts.append(f"===== Фрагмент {i + 1}/{total} =====\n{p.strip()}\n\n")
    user_parts.append("Склей фрагменты в единый конспект, сохранив все детали.")
    return [
        {"role": "system", "content": REDUCE_SYSTEM_PROMPT},
        {"role": "user", "content": "".join(user_parts)},
    ]


# ── note assembly ─────────────────────────────────────────────────────────────

def build_note(
    title: str,
    duration: float,
    transcript: str,
    llm_body: str,
) -> str:
    """Build the full Obsidian note: frontmatter + LLM body + collapsed transcript."""
    body = _strip_image_embeds(llm_body.strip())

    lines = [
        "---",
        f"title: {title}",
        "tags: [видео, конспект]",
        f"duration: {duration:.0f}s",
        "---",
        "",
        f"# {title}",
        "",
        body.strip(),
        "",
        "> [!note]- Полный транскрипт",
    ]
    for line in transcript.splitlines():
        lines.append(f"> {line}")

    return "\n".join(lines) + "\n"


# ── audio extraction seam (stubbable in tests) ────────────────────────────────

async def extract_audio_from_file(ctx, file) -> bytes:
    """Extract audio bytes from `file.bytes` (video file).

    This function is a seam: tests monkeypatch it to return stub audio.
    Production path: write bytes to a tempfile and call video_helpers.extract_audio.
    """
    import sys
    # video_helpers is at the toolgate root; it may not be on sys.path in tests
    tg_root = str(__import__("pathlib").Path(__file__).resolve().parents[3])
    if tg_root not in sys.path:
        sys.path.insert(0, tg_root)
    try:
        from video_helpers import extract_audio  # type: ignore[import]
    except ImportError as exc:
        raise RuntimeError(
            f"video_helpers not importable from toolgate root: {exc}"
        ) from exc

    with tempfile.NamedTemporaryFile(
        suffix=".mp4", delete=False
    ) as tmp:
        tmp.write(file.bytes)
        tmp_path = tmp.name
    try:
        return await extract_audio(tmp_path)
    finally:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass


# ── title extraction ──────────────────────────────────────────────────────────

def _extract_title(filename: str) -> str:
    """Derive a human title from the source filename (strip extension)."""
    base = os.path.basename(filename or "video")
    stem, _ = os.path.splitext(base)
    return stem or "video"


def _extract_llm_title(llm_body: str, fallback: str) -> str:
    """Try to extract a short title from the LLM body (first H1 or H2 after Конспект)."""
    # Use fallback — title extraction from LLM body is fragile; rely on filename
    return fallback


# ── main handler ─────────────────────────────────────────────────────────────

async def run(ctx, file, params):
    """Full LLM map-reduce digest for video files.

    Steps:
      1. fetch audio (from file.bytes via extract_audio_from_file seam)
      2. transcribe via ctx.stt
      3. digest (single-pass or map-reduce) via ctx.llm
      4. assemble Obsidian note + return with obsidian_note post_action
    """
    language = params.get("language", "ru")

    # ── 1. fetch ─────────────────────────────────────────────────────────────
    await ctx.progress("fetch", 10)
    if file.bytes:
        audio = await extract_audio_from_file(ctx, file)
    elif file.source_url:
        # url-based job: best-effort download then extract
        import tempfile as _tf
        import sys as _sys
        tg_root = str(__import__("pathlib").Path(__file__).resolve().parents[3])
        if tg_root not in _sys.path:
            _sys.path.insert(0, tg_root)
        try:
            from video_helpers import download_video, extract_audio  # type: ignore[import]
            with _tf.TemporaryDirectory() as d:
                path = await download_video(file.source_url, d)
                audio = await extract_audio(path)
        except Exception as exc:
            return ctx.result.failed(f"source_url fetch failed: {exc}")
    else:
        return ctx.result.failed("no file bytes or source_url provided")

    # ── 2. transcribe ─────────────────────────────────────────────────────────
    await ctx.progress("transcribe", 30)
    audio_filename = (file.filename or "video.mp4").replace(
        os.path.splitext(file.filename or "")[1] or ".mp4", ".ogg"
    )
    transcript = await ctx.stt.transcribe(
        audio,
        filename=audio_filename,
        language=language,
    )

    # ── 3. digest ─────────────────────────────────────────────────────────────
    await ctx.progress("digest", 50)

    if should_chunk(transcript):
        # Map-reduce path (long video: > DIGEST_CHUNK_THRESHOLD_MIN minutes)
        chunks = split_transcript_by_time(transcript, DIGEST_CHUNK_MINUTES)
        if len(chunks) >= 2:
            sem = asyncio.Semaphore(4)

            async def _map_chunk(chunk: TranscriptChunk, idx: int) -> str:
                async with sem:
                    msgs = build_chunk_messages(chunk, idx, len(chunks))
                    return await ctx.llm.complete(msgs)

            partials = await asyncio.gather(
                *[_map_chunk(c, i) for i, c in enumerate(chunks)]
            )
            # reduce
            reduce_msgs = build_reduce_messages(list(partials))
            llm_body = await ctx.llm.complete(reduce_msgs)
        else:
            # Degenerate: only one chunk despite should_chunk=True; go single-pass
            msgs = build_single_pass_messages(transcript)
            llm_body = await ctx.llm.complete(msgs)
    else:
        # Single-pass path (short video)
        msgs = build_single_pass_messages(transcript)
        llm_body = await ctx.llm.complete(msgs)

    # ── 4. assemble note + return ─────────────────────────────────────────────
    await ctx.progress("saving", 90)

    title = _extract_title(file.filename or "video.mp4")
    # Build Latin-safe slug for the filename (core path guard rejects Cyrillic)
    content_hash = hashlib.sha256(transcript.encode()).hexdigest()[:8]
    slug = latin_slug(title, content_hash)

    note = build_note(
        title=title,
        duration=0.0,  # duration not available from bytes-only path; 0 is safe
        transcript=transcript,
        llm_body=llm_body,
    )

    # Extract short summary text from the LLM body for the status message
    summary_text = _extract_short_summary(llm_body)

    result = ctx.result.text(summary_text)
    result.post_action = {
        "kind": "obsidian_note",
        "folder": "Summary",
        "filename": f"{slug}.md",
        "content": note,
    }
    return result


def _extract_short_summary(llm_body: str) -> str:
    """Extract first paragraph of ## Резюме, or first non-empty line."""
    if "## Резюме" in llm_body:
        after = llm_body[llm_body.find("## Резюме") + len("## Резюме"):]
        # Take everything up to the next ## section
        body = after.split("\n## ")[0].strip()
        # First non-empty line
        for line in body.splitlines():
            line = line.strip()
            if line:
                return line
    for line in llm_body.splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            return line
    return "Конспект готов."
