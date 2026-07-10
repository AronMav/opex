"""TDD tests for term_fixer (spec: 2026-07-10-transcript-term-correction)."""
from __future__ import annotations

import sys
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import term_fixer as tf  # noqa: E402


# ── parse_detect_json ────────────────────────────────────────────────────────

def test_parse_plain_array():
    assert tf.parse_detect_json('[{"heard": "x"}]') == [{"heard": "x"}]


def test_parse_fenced_array():
    raw = '```json\n[{"heard": "x"}]\n```'
    assert tf.parse_detect_json(raw) == [{"heard": "x"}]


def test_parse_candidates_object():
    assert tf.parse_detect_json('{"candidates": [{"heard": "x"}]}') == [{"heard": "x"}]


def test_parse_trailing_text_extracts_first_block():
    raw = 'Вот JSON:\n[{"heard": "x"}]\nГотово.'
    assert tf.parse_detect_json(raw) == [{"heard": "x"}]


def test_parse_trailing_text_with_extra_brackets():
    # жадный [.*] захватил бы до ПОСЛЕДНЕЙ ] — raw_decode берёт первый валидный массив
    raw = '[{"heard": "x"}]\nГотово [я закончил].'
    assert tf.parse_detect_json(raw) == [{"heard": "x"}]


def test_parse_empty_array_is_honest_empty():
    assert tf.parse_detect_json("[]") == []


def test_parse_garbage_returns_none():
    # None = «не распарсилось» (оркестратор логирует warning); [] = «кандидатов нет»
    assert tf.parse_detect_json("тут нет никакого json") is None


def test_parse_non_list_json_returns_none():
    assert tf.parse_detect_json('{"heard": "x"}') is None  # объект без candidates


# ── normalize_candidate ──────────────────────────────────────────────────────

def _item(**over):
    base = {
        "heard": "амбассадор",
        "variants": ["амбассадора", "амбассадором"],
        "description": "суб-бас плагин",
        "query": "sub bass plugin ambassador",
    }
    base.update(over)
    return base


def test_normalize_happy_path_includes_heard_in_variants():
    c = tf.normalize_candidate(_item())
    assert c is not None
    assert "амбассадор" in c["variants"]
    assert "амбассадора" in c["variants"]


def test_normalize_filters_empty_short_and_digit_variants():
    c = tf.normalize_candidate(_item(variants=["", "  ", "ам", "37", "12-37", "амбассадора"]))
    assert c is not None
    assert c["variants"] == ["амбассадор", "амбассадора"]


def test_normalize_caps_variants():
    many = [f"вариант{i:02d}" for i in range(20)]
    c = tf.normalize_candidate(_item(variants=many))
    assert len(c["variants"]) <= tf.MAX_VARIANTS


def test_normalize_rejects_digit_only_heard():
    # heard тоже словоформа: чисто цифровой heard без выживших variants → None
    assert tf.normalize_candidate(_item(heard="37", variants=[])) is None


def test_normalize_rejects_multiline_query():
    assert tf.normalize_candidate(_item(query="line1\nline2")) is None


def test_normalize_rejects_overlong_query():
    assert tf.normalize_candidate(_item(query="x" * (tf.MAX_QUERY_LEN + 1))) is None


def test_normalize_rejects_missing_keys():
    assert tf.normalize_candidate({"heard": "x"}) is None


def test_normalize_rejects_non_string_values():
    assert tf.normalize_candidate(_item(heard=42)) is None
    assert tf.normalize_candidate(_item(description=None)) is None
