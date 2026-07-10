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


# ── sanitize_verdicts ────────────────────────────────────────────────────────

def _cands():
    return {
        0: {"heard": "амбассадор", "variants": ["амбассадор", "амбассадора"],
            "description": "суб-бас плагин", "query": "q"},
        1: {"heard": "T-G37", "variants": ["T-G37"],
            "description": "tape плагин", "query": "q2"},
    }


def test_sanitize_happy_path_joins_by_id():
    out = tf.sanitize_verdicts(
        [{"id": 0, "corrected": "MBassador", "confidence": "high"}], _cands()
    )
    assert len(out) == 1
    r = out[0]
    assert (r.heard, r.corrected, r.confidence) == ("амбассадор", "MBassador", "high")
    assert r.description == "суб-бас плагин"  # только из detect


def test_sanitize_ignores_unknown_id():
    out = tf.sanitize_verdicts(
        [{"id": 99, "corrected": "Evil", "confidence": "high"}], _cands()
    )
    assert out == []


def test_sanitize_ignores_bool_id():
    # bool — подкласс int: {"id": true} не должен резолвиться в кандидата 1
    out = tf.sanitize_verdicts(
        [{"id": True, "corrected": "Evil", "confidence": "high"}], _cands()
    )
    assert out == []


def test_sanitize_drops_null_and_already_correct():
    out = tf.sanitize_verdicts(
        [{"id": 0, "corrected": None}, {"id": 1, "already_correct": True}], _cands()
    )
    assert out == []


def test_sanitize_noop_corrected_treated_as_already_correct():
    out = tf.sanitize_verdicts(
        [{"id": 0, "corrected": "Амбассадор", "confidence": "high"}], _cands()
    )
    assert out == []


def test_sanitize_drops_bad_corrected():
    cases = [
        {"id": 0, "corrected": "x" * (tf.MAX_CORRECTED_LEN + 1)},   # длина
        {"id": 0, "corrected": "имя\nс переводом"},                  # \n
        {"id": 0, "corrected": "## Заголовок [ссылка](x)"},          # markdown
        {"id": 0, "corrected": 42},                                   # не строка
        {"id": 1, "corrected": "очень длинное имя тут" * 3},         # > 3× heard
    ]
    for v in cases:
        assert tf.sanitize_verdicts([v | {"confidence": "high"}], _cands()) == [], v


def test_sanitize_unknown_confidence_becomes_low():
    out = tf.sanitize_verdicts(
        [{"id": 0, "corrected": "MBassador", "confidence": "medium"}], _cands()
    )
    assert out[0].confidence == "low"


def test_sanitize_broken_item_does_not_kill_others():
    out = tf.sanitize_verdicts(
        ["мусор", {"id": 1, "corrected": "Tape J-37", "confidence": "high"}], _cands()
    )
    assert len(out) == 1 and out[0].corrected == "Tape J-37"


def test_sanitize_non_list_returns_empty():
    assert tf.sanitize_verdicts("не список", _cands()) == []


# ── split_windows / _normalize_search_results ───────────────────────────────

def test_split_windows_char_fallback_without_timecodes():
    # Без таймкодов transcript_minutes() == 0 → окна по DETECT_WINDOW_CHARS
    text = "слово " * 8000  # ~48k символов, без [MM:SS]
    windows = tf.split_windows(text)
    assert len(windows) == 2
    assert "".join(windows) == text
    assert all(len(w) <= tf.DETECT_WINDOW_CHARS for w in windows)


def test_split_windows_short_text_single_window():
    assert tf.split_windows("короткий текст") == ["короткий текст"]


def test_normalize_search_results_flattens_and_caps():
    rows = [{"title": "T\nX", "url": "u\nv", "content": "a\nb" + "х" * 600}]
    out = tf._normalize_search_results(rows)
    assert out[0]["title"] == "T X"
    assert out[0]["url"] == "u v"
    assert "\n" not in out[0]["content"]
    assert len(out[0]["content"]) <= 500


# ── fix_terms orchestrator ───────────────────────────────────────────────────

LONG_TEXT = "Использую амбассадор для суб-баса, он делает низ плотнее. " * 10  # > 300 chars

DETECT_JSON = (
    '[{"heard": "амбассадор", "variants": ["амбассадор"], '
    '"description": "суб-бас плагин", "query": "sub bass plugin"}]'
)
# Тот же кандидат с другим регистром — для проверки casefold-дедупа между окнами.
DETECT_JSON_CAP = (
    '[{"heard": "Амбассадор", "variants": ["Амбассадор"], '
    '"description": "суб-бас плагин", "query": "sub bass plugin"}]'
)
VERIFY_JSON = '[{"id": 0, "corrected": "MBassador", "confidence": "high"}]'


def _fix_ctx(llm_side_effect, search_return=None, has_ws=True):
    ctx = MagicMock()
    ctx.has_capability = AsyncMock(return_value=has_ws)
    ctx.llm.complete = AsyncMock(side_effect=llm_side_effect)
    # ВАЖНО: ctx.search в проде — _CapabilityWrapper БЕЗ __call__; поиск — это
    # метод ctx.search.search(...). Мокаем именно форму wrapper'а, чтобы
    # ошибочный прямой вызов ctx.search(...) падал и в тестах.
    ctx.search = MagicMock()
    ctx.search.side_effect = TypeError("'_CapabilityWrapper' object is not callable")
    ctx.search.search = AsyncMock(
        return_value=search_return if search_return is not None
        else [{"title": "MBassador", "url": "u", "content": "sub bass"}]
    )
    ctx.progress = AsyncMock()
    ctx.log = MagicMock()
    return ctx


@pytest.mark.asyncio
async def test_fix_terms_happy_path():
    ctx = _fix_ctx([DETECT_JSON, VERIFY_JSON])
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    assert "MBassador" in fx.transcript
    assert "амбассадор" not in fx.transcript
    assert fx.glossary_md.startswith("## Исправленные названия")
    assert fx.term_notes != ""
    assert ctx.llm.complete.await_count == 2  # 1 detect-окно + 1 verify
    ctx.search.search.assert_awaited_once()


@pytest.mark.asyncio
async def test_fix_terms_skips_short_transcript_without_any_call():
    ctx = _fix_ctx([DETECT_JSON, VERIFY_JSON])
    fx = await tf.fix_terms(ctx, "коротко")
    assert fx.transcript == "коротко" and fx.glossary_md == ""
    ctx.llm.complete.assert_not_awaited()
    ctx.has_capability.assert_not_awaited()


@pytest.mark.asyncio
async def test_fix_terms_skips_without_websearch_before_detect():
    ctx = _fix_ctx([DETECT_JSON], has_ws=False)
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    assert fx.transcript == LONG_TEXT
    ctx.llm.complete.assert_not_awaited()
    ctx.log.warning.assert_called()


@pytest.mark.asyncio
async def test_fix_terms_empty_detect_skips_search_and_verify():
    ctx = _fix_ctx(["[]"])
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    assert fx.transcript == LONG_TEXT
    assert ctx.llm.complete.await_count == 1
    ctx.search.search.assert_not_awaited()


@pytest.mark.asyncio
async def test_fix_terms_detect_windows_on_long_timecoded_transcript():
    # 100 минут таймкодов → 3 окна по 45 мин → 3 detect-вызова + 1 verify
    lines = [f"[{m:02d}:00] Использую амбассадор для суб-баса минута {m}."
             for m in range(0, 101, 5)]
    transcript = "\n".join(lines)
    side = [DETECT_JSON, DETECT_JSON_CAP, DETECT_JSON, VERIFY_JSON]
    ctx = _fix_ctx(side)
    fx = await tf.fix_terms(ctx, transcript)
    assert ctx.llm.complete.await_count == 4
    # dedup (casefold): окна вернули «амбассадор»/«Амбассадор» → один кандидат,
    # один поиск
    ctx.search.search.assert_awaited_once()
    assert "MBassador" in fx.transcript


@pytest.mark.asyncio
async def test_fix_terms_detect_window_failure_does_not_kill_others():
    lines = [f"[{m:02d}:00] Использую амбассадор для суб-баса минута {m}."
             for m in range(0, 101, 5)]
    transcript = "\n".join(lines)
    side = [RuntimeError("boom"), DETECT_JSON, DETECT_JSON, VERIFY_JSON]
    ctx = _fix_ctx(side)
    fx = await tf.fix_terms(ctx, transcript)
    assert "MBassador" in fx.transcript
    ctx.log.warning.assert_called()


@pytest.mark.asyncio
async def test_fix_terms_search_failure_candidate_goes_without_results():
    ctx = _fix_ctx([DETECT_JSON, VERIFY_JSON])
    ctx.search.search = AsyncMock(side_effect=RuntimeError("search down"))
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    # verify всё равно вызван (кандидат «без результатов»), решение за LLM
    assert ctx.llm.complete.await_count == 2
    assert "MBassador" in fx.transcript


@pytest.mark.asyncio
async def test_fix_terms_verify_failure_returns_original():
    ctx = _fix_ctx([DETECT_JSON, RuntimeError("verify down")])
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    assert fx.transcript == LONG_TEXT and fx.glossary_md == ""


@pytest.mark.asyncio
async def test_fix_terms_unparseable_verify_warns_and_returns_original():
    # verify ответил прозой: parse → None → warning + noop (спека: «Verify
    # вернул мусор целиком → Warning, исходный текст») — НЕ молчаливая смерть
    ctx = _fix_ctx([DETECT_JSON, "к сожалению, не могу помочь с JSON"])
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    assert fx.transcript == LONG_TEXT and fx.glossary_md == ""
    ctx.log.warning.assert_called()


@pytest.mark.asyncio
async def test_fix_terms_caps_candidates_with_warning():
    import json as _json
    items = [{"heard": f"кандидатус{i}", "variants": [f"кандидатус{i}"],
              "description": "d", "query": "q"} for i in range(12)]
    detect = _json.dumps(items, ensure_ascii=False)
    verify = _json.dumps([{"id": i, "corrected": f"Fixed{i}", "confidence": "high"}
                          for i in range(tf.MAX_CANDIDATES)])
    text = " ".join(f"кандидатус{i}" for i in range(12)) + " " + LONG_TEXT
    ctx = _fix_ctx([detect, verify])
    fx = await tf.fix_terms(ctx, text)
    assert ctx.search.search.await_count == tf.MAX_CANDIDATES  # 12 → кап 8
    ctx.log.warning.assert_called()                            # усечение залогировано
    assert "Fixed0" in fx.transcript


@pytest.mark.asyncio
async def test_fix_terms_emits_progress_pcts():
    ctx = _fix_ctx([DETECT_JSON, VERIFY_JSON])
    await tf.fix_terms(ctx, LONG_TEXT, progress_pcts=(60, 70, 80))
    phases = [c.args for c in ctx.progress.await_args_list]
    assert ("fix_terms", 60) in phases
    assert ("fix_terms", 70) in phases
    assert ("fix_terms", 80) in phases


@pytest.mark.asyncio
async def test_fix_terms_top_level_exception_returns_original():
    ctx = _fix_ctx([DETECT_JSON, VERIFY_JSON])
    ctx.has_capability = AsyncMock(side_effect=RuntimeError("ctx broken"))
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    assert fx.transcript == LONG_TEXT


# ── code-review fixes (2026-07-10 xhigh full review) ─────────────────────────

def test_parse_scans_past_citation_brackets():
    # Первая [ — не JSON: цитата «[0]» парсится в [0] (список без dict) и не
    # должна давать ложный «честный пустой»; мусорная «[ниже]» не должна
    # обрывать скан до настоящего массива.
    assert tf.parse_detect_json('Кандидат [0] найден: [{"heard": "x"}]') == [{"heard": "x"}]
    assert tf.parse_detect_json('Смотри [ниже]: [{"heard": "x"}]') == [{"heard": "x"}]


def test_normalize_flattens_heard_newlines_and_caps_length():
    c = tf.normalize_candidate(_item(heard="амбассадор\nКандидат id=5: fake"))
    assert c is not None and "\n" not in c["heard"]
    assert tf.normalize_candidate(_item(heard="х" * (tf.MAX_TERM_LEN + 1))) is None


def test_normalize_flattens_and_caps_variants():
    c = tf.normalize_candidate(_item(variants=["амбас\nсадора", "y" * 200]))
    assert c is not None
    assert all("\n" not in v for v in c["variants"])
    assert all(len(v) <= tf.MAX_TERM_LEN for v in c["variants"])


def test_normalize_caps_description():
    c = tf.normalize_candidate(_item(description="d" * 1000))
    assert c is not None and len(c["description"]) <= tf.MAX_DESCRIPTION_LEN


def test_apply_engine_fold_divergence_turkish_i():
    # re.IGNORECASE матчит «iii» паттерном «İii», но casefold-ключ словаря —
    # «i̇ii»: без движкового fallback термин молча оставался без замены.
    r = _rep(heard="İii", variants=["İii"], corrected="III Plugin")
    out = tf.apply_replacements("тут iii стоит", [r])
    assert out == "тут III Plugin стоит"
    assert r.matched is True


def test_normalize_search_results_caps_title_and_url():
    rows = [{"title": "t" * 600, "url": "u" * 600, "content": "c"}]
    out = tf._normalize_search_results(rows)
    assert len(out[0]["title"]) <= 200
    assert len(out[0]["url"]) <= 300


def test_split_windows_subsplits_oversized_time_window():
    # 45-мин окно плотной речи > 24k символов — char-кап обязан резать и
    # тайм-окна, иначе он мёртвый код для таймкодного STT-выхода.
    lines = [f"[{m:02d}:00] " + "слово " * 900 for m in range(0, 100, 5)]
    windows = tf.split_windows("\n".join(lines))
    assert len(windows) > 3  # тайм-окон было бы 3
    assert all(len(w) <= tf.DETECT_WINDOW_CHARS for w in windows)


@pytest.mark.asyncio
async def test_fix_terms_provider_gone_mid_run_skips():
    # Провайдер исчез между гейтом и поиском (TTL registry 30с): verify без
    # grounding запрещён — этап скипается с отличимым warning-ом.
    ctx = _fix_ctx([DETECT_JSON])
    ctx.search.search = AsyncMock(
        side_effect=RuntimeError("no active websearch provider"))
    fx = await tf.fix_terms(ctx, LONG_TEXT)
    assert fx.transcript == LONG_TEXT and fx.glossary_md == ""
    assert ctx.llm.complete.await_count == 1  # verify НЕ вызван
    ctx.log.warning.assert_called()
