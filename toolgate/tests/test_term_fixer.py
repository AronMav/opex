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


# ── apply_replacements ───────────────────────────────────────────────────────

def _rep(heard="амбассадор", variants=None, corrected="MBassador",
         confidence="high", description="суб-бас плагин"):
    return tf.Replacement(
        heard=heard,
        variants=variants if variants is not None else [heard],
        corrected=corrected,
        confidence=confidence,
        description=description,
    )


def test_apply_high_replaces_all_occurrences():
    text = "Возьмём амбассадор. Потом амбассадор снова."
    r = _rep()
    out = tf.apply_replacements(text, [r])
    assert out == "Возьмём MBassador. Потом MBassador снова."
    assert r.matched is True


def test_apply_respects_word_boundaries():
    r = _rep(heard="тейп", variants=["тейп"], corrected="Tape")
    out = tf.apply_replacements("тейповый саунд и тейп рядом", [r])
    assert out == "тейповый саунд и Tape рядом"


def test_apply_is_case_insensitive():
    r = _rep()
    out = tf.apply_replacements("Амбассадор хорош. И амбассадор тоже.", [r])
    assert out == "MBassador хорош. И MBassador тоже."


def test_apply_single_pass_no_cascade():
    # corrected первого кандидата СОДЕРЖИТ словоформу второго ("Tape") — при
    # последовательных str.replace второй прошёлся бы по уже подставленному
    # тексту ("Tape Machine J-37"); однопроходный re.sub это запрещает.
    r1 = _rep(heard="Tape G37", variants=["Tape G37"], corrected="Tape J-37")
    r2 = _rep(heard="Tape", variants=["Tape"], corrected="Tape Machine")
    out = tf.apply_replacements("use Tape G37 and Tape here", [r1, r2])
    assert out == "use Tape J-37 and Tape Machine here"


def test_apply_low_annotates_only_first_occurrence():
    r = _rep(confidence="low")
    text = "амбассадор раз. амбассадор два."
    out = tf.apply_replacements(text, [r])
    assert out == "амбассадор (вероятно MBassador?) раз. амбассадор два."


def test_apply_low_annotation_localized_en():
    r = _rep(confidence="low")
    out = tf.apply_replacements("амбассадор здесь", [r], language="en")
    assert out == "амбассадор (likely MBassador?) здесь"


def test_apply_longest_variant_wins():
    r = _rep(variants=["амбассадора", "амбассадор"])
    out = tf.apply_replacements("без амбассадора никуда", [r])
    assert out == "без MBassador никуда"


def test_apply_preserves_timecodes_url_segments_not_guaranteed():
    r = _rep(heard="тест", variants=["тест"], corrected="Test")
    text = "[12:37] тест на https://example.com/тест-page"
    out = tf.apply_replacements(text, [r])
    assert "[12:37]" in out                      # таймкод не тронут
    assert out.startswith("[12:37] Test на ")
    # ПРИНЯТОЕ ОГРАНИЧЕНИЕ (зафиксировано в спеке): словоформа внутри
    # URL-сегмента между не-\w символами ("/тест-") ЗАМЕНЯЕТСЯ — защита URL
    # не гарантируется, защищены только таймкоды (фильтром цифровых variants).


def test_apply_atomic_rollback_resets_matched():
    # corrected=None провоцирует TypeError внутри re.sub (repl вернул не-str):
    # текст откатывается ЦЕЛИКОМ и matched-флаги сбрасываются — иначе
    # fix_terms вернул бы исходный транскрипт с глоссарием, лгущим о заменах.
    bad = _rep(corrected=None)  # type: ignore[arg-type]
    out = tf.apply_replacements("амбассадор здесь", [bad])
    assert out == "амбассадор здесь"
    assert bad.matched is False


def test_apply_unmatched_variant_leaves_matched_false():
    r = _rep(heard="фантом", variants=["фантом"])
    out = tf.apply_replacements("здесь ничего похожего нет", [r])
    assert out == "здесь ничего похожего нет"
    assert r.matched is False


def test_apply_empty_reps_returns_text():
    assert tf.apply_replacements("текст", []) == "текст"


# ── glossary / term_notes ────────────────────────────────────────────────────

def test_glossary_empty_without_matched():
    r = _rep()  # matched=False по умолчанию
    assert tf.build_glossary([r]) == ""


def test_glossary_high_and_low_rows_ru():
    hi = _rep(); hi.matched = True
    lo = _rep(heard="T-G37", variants=["T-G37"], corrected="Arturia Tape J-37",
              confidence="low", description=""); lo.matched = True
    g = tf.build_glossary([hi, lo])
    assert g.startswith("## Исправленные названия")
    assert "- «амбассадор» → **MBassador** — суб-бас плагин" in g
    assert "*вероятно* **Arturia Tape J-37** (не подтверждено)" in g


def test_glossary_localized_en():
    hi = _rep(); hi.matched = True
    g = tf.build_glossary([hi], language="en")
    assert g.startswith("## Corrected names")


def test_glossary_escapes_markdown_and_flattens_newlines():
    r = _rep(heard="a*b", variants=["a*b"], corrected="C_name",
             description="line1\nline2 [x](y)")
    r.matched = True
    g = tf.build_glossary([r])
    assert "a\\*b" in g
    assert "C\\_name" in g
    assert "\nline2" not in g          # description сплющен в одну строку
    assert "\\[x\\]" in g


def test_term_notes_high_and_low_ru():
    hi = _rep(); hi.matched = True
    lo = _rep(heard="T-G37", variants=["T-G37"], corrected="Tape J-37",
              confidence="low"); lo.matched = True
    n = tf.build_term_notes([hi, lo])
    assert n.startswith("В транскрипте уже исправлены названия:")
    assert '"MBassador" (было "амбассадор")' in n
    assert '"T-G37" вероятно означает "Tape J-37"' in n
    assert "вероятно" in n


def test_term_notes_localized_en():
    hi = _rep(); hi.matched = True
    n = tf.build_term_notes([hi], language="en")
    assert n.startswith("Product names were already corrected in the transcript:")
    assert '"MBassador" (was "амбассадор")' in n


def test_term_notes_empty_without_matched():
    assert tf.build_term_notes([_rep()]) == ""
