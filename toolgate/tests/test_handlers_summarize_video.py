"""TDD tests for the full summarize_video handler (Phase 5, Task 6c).

Tests:
  (a) descriptor: execution=async, matches video/*
  (b) short transcript (<= 150 min) → single-pass (ctx.llm called ONCE)
      → post_action.kind == "obsidian_note", filename ends ".md",
        filename matches Latin-safe pattern, progress includes "digest"
  (c) long transcript (> 150 min via crafted [MM:SS] markers)
      → map-reduce: ctx.llm called >= 3 times (N chunk maps + 1 reduce)
"""
from __future__ import annotations

import re
import sys
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from handlers.builtin import summarize_video as sv_mod  # noqa: E402
from handlers.context import HandlerFile, HandlerResult  # noqa: E402


# ── helpers ──────────────────────────────────────────────────────────────────

LATIN_SAFE_RE = re.compile(r"^[A-Za-z0-9 _.\-]+$")


def _make_ctx(llm_side_effect=None):
    """Build a minimal mock ctx with stt, llm, progress, result, and log."""
    ctx = MagicMock()

    # progress is async
    ctx.progress = AsyncMock()

    # stt.transcribe returns a short transcript by default
    ctx.stt.transcribe = AsyncMock(return_value="Hello world.\nSimple transcript.")

    # llm.complete returns a fake digest
    if llm_side_effect is not None:
        ctx.llm.complete = AsyncMock(side_effect=llm_side_effect)
    else:
        ctx.llm.complete = AsyncMock(
            return_value=(
                "## Резюме\nКороткое видео.\n\n## Конспект\n### Шаг 1\nДетали."
            )
        )

    # result builder
    ctx.result = MagicMock()
    ctx.result.text = MagicMock(
        side_effect=lambda s: HandlerResult(status="ok", summary_text=s)
    )
    ctx.result.failed = MagicMock(
        side_effect=lambda r: HandlerResult(status="failed", reason=r)
    )

    ctx.log = MagicMock()
    return ctx


def _video_file(b: bytes = b"FAKEVIDEO") -> HandlerFile:
    return HandlerFile(bytes=b, mime="video/mp4", filename="test.mp4", size=len(b))


def _make_long_transcript(total_minutes: int = 160) -> str:
    """Build a transcript whose last timecode is at `total_minutes` minutes."""
    lines = []
    for m in range(0, total_minutes + 1, 10):
        lines.append(f"[{m:02d}:00] Content at minute {m}.")
    return "\n".join(lines)


# ── stub for extract_audio_from_file ─────────────────────────────────────────

STUB_AUDIO = b"FAKEAUDIO"


async def _fake_extract_audio(ctx, file):  # noqa: D401
    return STUB_AUDIO


# ── (a) descriptor ───────────────────────────────────────────────────────────


def test_descriptor_is_async_and_matches_video():
    """The descriptor must declare execution=async and match video/*."""
    import os
    from handlers.loader import HandlerRegistry

    reg = HandlerRegistry()
    builtin_dir = str(
        Path(__file__).resolve().parents[1] / "handlers" / "builtin"
    )
    reg.load_all(builtin_dir, None)
    lh = reg.get("summarize_video")
    assert lh is not None, "summarize_video not registered"
    d = lh.descriptor
    assert d.execution == "async", f"expected async, got {d.execution!r}"
    assert any("video" in m for m in d.match_mimes), (
        f"no video/* mime match in {d.match_mimes}"
    )
    # Operator-configurable summary_folder valve is declared and parsed.
    folder_field = next((c for c in d.config if c["name"] == "summary_folder"), None)
    assert folder_field is not None, f"no summary_folder <config> field in {d.config}"
    assert folder_field["default"] == "Summary"


# ── (b) short transcript → single-pass ───────────────────────────────────────


@pytest.mark.asyncio
async def test_short_transcript_single_pass_llm_called_once():
    """Short transcript (<= 150 min) → single-pass: ctx.llm.complete called ONCE."""
    ctx = _make_ctx()
    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {"language": "ru"})

    assert ctx.llm.complete.call_count == 1, (
        f"Expected 1 llm call, got {ctx.llm.complete.call_count}"
    )


@pytest.mark.asyncio
async def test_empty_transcript_fails_without_hallucinating():
    """Empty STT transcript → failed result; the digest LLM is NEVER called.

    Regression guard: a music-only video (or language mismatch) yields an empty
    transcript. Previously the handler digested it and the LLM hallucinated a
    fabricated summary that was delivered as if real."""
    ctx = _make_ctx()
    ctx.stt.transcribe = AsyncMock(return_value="")
    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {"language": "ru"})

    assert result.status == "failed", f"expected failed, got {result.status!r}"
    assert ctx.llm.complete.call_count == 0, (
        "digest LLM must not run on an empty transcript (hallucination guard)"
    )


@pytest.mark.asyncio
async def test_whitespace_and_timecode_only_transcript_fails():
    """A transcript that is only timecodes/whitespace is treated as empty."""
    ctx = _make_ctx()
    ctx.stt.transcribe = AsyncMock(return_value="[00:00]   \n[00:05]  \n")
    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {"language": "ru"})

    assert result.status == "failed", f"expected failed, got {result.status!r}"
    assert ctx.llm.complete.call_count == 0


@pytest.mark.asyncio
async def test_short_transcript_has_obsidian_note_post_action():
    """Single-pass result must have post_action with kind='obsidian_note'."""
    ctx = _make_ctx()
    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {"language": "ru"})

    assert hasattr(result, "post_action"), "HandlerResult must have post_action"
    pa = result.post_action
    assert pa is not None, "post_action must be set"
    assert pa["kind"] == "obsidian_note", f"kind={pa['kind']!r}"
    assert pa["folder"] == "Summary"
    fn = pa["filename"]
    assert fn.endswith(".md"), f"filename must end with .md: {fn!r}"


@pytest.mark.asyncio
async def test_short_transcript_filename_is_latin_safe():
    """Filename must match ^[A-Za-z0-9 _.-]+$ (core path guard)."""
    ctx = _make_ctx()
    # Simulate a Cyrillic title returned in the LLM body — slug must transliterate
    ctx.llm.complete = AsyncMock(
        return_value=(
            "## Резюме\nКирилличное видео.\n\n## Конспект\n### Шаг 1\nДетали."
        )
    )
    # Use a Cyrillic filename for the source file to trigger transliteration
    file = HandlerFile(
        bytes=b"FAKEVIDEO",
        mime="video/mp4",
        filename="Кириллица тест.mp4",
        size=9,
    )

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {"language": "ru"})

    fn = result.post_action["filename"]
    # Strip the .md suffix for the pattern check
    stem = fn[:-3] if fn.endswith(".md") else fn
    assert LATIN_SAFE_RE.match(stem) or LATIN_SAFE_RE.match(fn), (
        f"filename not Latin-safe: {fn!r}"
    )


@pytest.mark.asyncio
async def test_progress_includes_digest_phase():
    """progress() must be called with 'digest' phase."""
    ctx = _make_ctx()
    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        await sv_mod.run(ctx, file, {})

    called_phases = [call.args[0] for call in ctx.progress.call_args_list]
    assert "digest" in called_phases, (
        f"'digest' phase not found in progress calls: {called_phases}"
    )


@pytest.mark.asyncio
async def test_post_action_included_in_to_dict():
    """HandlerResult.to_dict() must include post_action when set."""
    ctx = _make_ctx()
    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {})

    d = result.to_dict()
    assert "post_action" in d, "to_dict() must include post_action"
    assert d["post_action"]["kind"] == "obsidian_note"


# ── (c) long transcript → map-reduce ─────────────────────────────────────────


@pytest.mark.asyncio
async def test_long_transcript_map_reduce_llm_called_multiple_times():
    """Long transcript (> 150 min) → map-reduce: ctx.llm.complete called >= 3 times."""
    # 160-minute transcript → should_chunk → True → multiple chunks
    long_transcript = _make_long_transcript(160)

    ctx = _make_ctx()
    # Override STT to return the long transcript
    ctx.stt.transcribe = AsyncMock(return_value=long_transcript)
    # Each LLM call returns a partial conspect
    ctx.llm.complete = AsyncMock(
        return_value="### Фрагмент\nДетали фрагмента конспекта."
    )
    # Last call (reduce) returns the merged result
    reduce_result = (
        "## Резюме\nВсё видео целиком.\n\n## Конспект\n### Общая тема\nДетали."
    )
    # Make the last call return the reduce result
    call_results = []

    async def side_effect(messages, **kwargs):
        call_results.append(messages)
        if len(call_results) == 1:
            return "### Фрагмент 1\nПервый фрагмент."
        elif len(call_results) == 2:
            return "### Фрагмент 2\nВторой фрагмент."
        elif len(call_results) == 3:
            return "### Фрагмент 3\nТретий фрагмент."
        else:
            return reduce_result

    ctx.llm.complete = AsyncMock(side_effect=side_effect)

    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {"language": "ru"})

    n_calls = ctx.llm.complete.call_count
    assert n_calls >= 3, (
        f"Expected >= 3 llm calls for map-reduce (chunks + reduce), got {n_calls}"
    )


@pytest.mark.asyncio
async def test_long_transcript_has_obsidian_note_post_action():
    """Map-reduce result also produces obsidian_note post_action."""
    long_transcript = _make_long_transcript(160)
    ctx = _make_ctx()
    ctx.stt.transcribe = AsyncMock(return_value=long_transcript)
    ctx.llm.complete = AsyncMock(
        return_value=(
            "## Резюме\nИтог.\n\n## Конспект\n### Раздел\nТекст."
        )
    )

    file = _video_file()

    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        result = await sv_mod.run(ctx, file, {"language": "ru"})

    assert hasattr(result, "post_action")
    pa = result.post_action
    assert pa["kind"] == "obsidian_note"
    fn = pa["filename"]
    assert fn.endswith(".md")


# ── unit helpers ──────────────────────────────────────────────────────────────


def test_transcript_minutes_parses_last_timecode():
    from handlers.builtin.summarize_video import transcript_minutes
    assert transcript_minutes("[00:04] раз\n[120:00] два") == 120
    assert transcript_minutes("[00:00] a\n[200:30] b") == 200
    assert transcript_minutes("без меток") == 0


def test_should_chunk_threshold():
    from handlers.builtin.summarize_video import should_chunk
    short = "[00:04] раз\n[150:00] два"  # exactly 150 → NOT > 150 → False
    assert not should_chunk(short), "exactly 150 min stays single-pass"
    long = "[00:00] a\n[151:00] b"
    assert should_chunk(long), "> 150 min uses chunked"


def test_split_transcript_by_time_windows():
    from handlers.builtin.summarize_video import split_transcript_by_time
    t = (
        "[00:10] a\n[10:00] b\n[44:59] c\nлиния без метки\n"
        "[46:00] d\n[89:00] e\n[91:00] f"
    )
    chunks = split_transcript_by_time(t, 45)
    assert len(chunks) == 3, f"expected 3 chunks, got {len(chunks)}"
    assert chunks[0].start_min == 0
    assert "[00:10] a" in chunks[0].text
    assert "линия без метки" in chunks[0].text
    assert chunks[1].start_min == 45
    assert "[91:00] f" in chunks[2].text


def test_latin_slug_from_cyrillic():
    from handlers.builtin.summarize_video import latin_slug
    s = latin_slug("Урок по Python", "abc")
    assert LATIN_SAFE_RE.match(s), f"not latin-safe: {s!r}"
    assert len(s) > 0


def test_latin_slug_fallback_on_empty():
    from handlers.builtin.summarize_video import latin_slug
    s = latin_slug("", "fallback123")
    assert "fallback" in s or LATIN_SAFE_RE.match(s)


def test_build_note_structure():
    from handlers.builtin.summarize_video import build_note
    note = build_note(
        title="Test Video",
        duration=120.0,
        transcript="[00:01] first line",
        llm_body="## Резюме\nШорт.\n\n## Конспект\n### Раздел\nДетали.",
    )
    assert note.startswith("---"), "must start with YAML frontmatter"
    assert "title: Test Video" in note
    assert "tags:" in note
    assert "# Test Video" in note
    assert "Полный транскрипт" in note
    assert "first line" in note
    # Image embeds should be stripped
    assert "![](" not in note


def test_build_note_strips_image_embeds():
    from handlers.builtin.summarize_video import build_note
    note = build_note(
        title="X",
        duration=60.0,
        transcript="text",
        llm_body="## Резюме\nR.\n\n## Конспект\n### S\nt\n![](images/f.jpg)\nmore",
    )
    assert "![](" not in note
    assert "more" in note


def test_strip_transcript_timecodes():
    from handlers.builtin.summarize_video import strip_transcript_timecodes
    result = strip_transcript_timecodes("[00:04] раз\n[131:20] два\nбез метки")
    assert result == "раз\nдва\nбез метки"


def test_build_single_pass_messages():
    from handlers.builtin.summarize_video import build_single_pass_messages
    msgs = build_single_pass_messages(
        transcript="[00:01] Hello world", duration=60.0
    )
    assert len(msgs) == 2
    assert msgs[0]["role"] == "system"
    assert msgs[1]["role"] == "user"
    # timecodes stripped
    assert "[00:01]" not in msgs[1]["content"]
    assert "Hello world" in msgs[1]["content"]


def test_build_chunk_messages():
    from handlers.builtin.summarize_video import (
        TranscriptChunk,
        build_chunk_messages,
    )
    chunk = TranscriptChunk(start_min=45, end_min=90, text="[46:00] середина")
    msgs = build_chunk_messages(chunk, idx=1, total=5)
    assert msgs[0]["role"] == "system"
    u = msgs[1]["content"]
    assert "Фрагмент 2 из 5" in u
    assert "минуты 45–90" in u
    assert "[46:00]" not in u
    assert "середина" in u


def test_build_reduce_messages():
    from handlers.builtin.summarize_video import build_reduce_messages
    partials = ["конспект A", "конспект B"]
    msgs = build_reduce_messages(partials)
    assert msgs[0]["role"] == "system"
    assert "СОХРАНИ ВСЕ" in msgs[0]["content"]
    u = msgs[1]["content"]
    a_pos = u.find("конспект A")
    b_pos = u.find("конспект B")
    assert 0 <= a_pos < b_pos, "partials must be in order"
    assert "Фрагмент 1/2" in u
    assert "Фрагмент 2/2" in u
