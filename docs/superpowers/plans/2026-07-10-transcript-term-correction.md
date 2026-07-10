# Transcript Term Correction (term_fixer) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Этап detect → search → verify → apply в обработчиках `transcribe` и `summarize_video`, исправляющий искажённые STT названия (бренды, плагины, термины) через веб-поиск по описанию.

**Architecture:** Новый модуль `toolgate/term_fixer.py` (чистые функции + оркестратор `fix_terms(ctx, ...)`), вызываемый обоими обработчиками после STT. LLM-детекция кандидатов по 45-минутным окнам, параллельный веб-поиск, батч-верификация с санитизацией, однопроходная regex-замена с границами слова. Новый probe `ctx.has_capability` в контракте handler-контекста. UI получает одну новую фазу прогресса.

**Tech Stack:** Python 3 (toolgate, pytest + pytest-asyncio), TypeScript/React (UI-фаза), без новых зависимостей.

**Спека:** `docs/superpowers/specs/2026-07-10-transcript-term-correction-design.md` — читать при любой неоднозначности.

## Global Constraints

- Константы модуля (значения из спеки, менять нельзя): `MIN_FIX_CHARS = 300`, `MAX_CANDIDATES = 8`, `MAX_VARIANTS = 10`, `MIN_VARIANT_LEN = 3`, `MAX_CORRECTED_LEN = 80`, `MAX_QUERY_LEN = 200`, `DETECT_WINDOW_MIN = 45`, `DETECT_WINDOW_CHARS = 24_000`, `SEARCH_CONCURRENCY = 4`.
- **Коррекция никогда не роняет джобу**: любая ошибка внутри `fix_terms` → `ctx.log.warning` + возврат исходного транскрипта.
- `corrected`/глоссарий — недоверенные данные (веб-сниппеты): санитизация обязательна, нарушение = отброс кандидата, не обрезка.
- НЕ копировать `parents[3]`-sys.path-шов из обработчиков (скрытый off-by-one); `term_fixer` импортируется `from term_fixer import fix_terms` внутри `run()` — корень toolgate кладёт в `sys.path` async-runner (`handlers/runner.py`).
- Ключ результатов веб-поиска — `content` (НЕ `snippet`).
- Веб-поиск — `ctx.search.search(query, max_results=5)`: `ctx.search` это `_CapabilityWrapper` БЕЗ `__call__` (как `ctx.stt.transcribe`); прямой вызов `ctx.search(...)` — TypeError.
- Тесты гоняются из каталога `toolgate/`: `python -m pytest tests/<file> -v`.
- Коммиты в master, без Claude-атрибуции, push только с явного разрешения.

## File Structure

| Файл | Ответственность |
| --- | --- |
| Create `toolgate/term_fixer.py` | Весь пайплайн: константы, dataclasses, парсер/фильтры detect, apply-движок, глоссарий/term_notes, санитизация verify, промпты, оркестратор `fix_terms` |
| Create `toolgate/tests/test_term_fixer.py` | Все unit-тесты модуля |
| Modify `toolgate/handlers/context.py` | + `HandlerContext.has_capability` |
| Modify `toolgate/tests/test_handlers_context.py` | + тесты probe |
| Modify `toolgate/handlers/builtin/transcribe.py` | Valve + вызов fix_terms + глоссарий после `---` |
| Modify `toolgate/handlers/builtin/summarize_video.py` | Valve + вызов fix_terms + `term_notes` в prompt-builders + `glossary` в `build_note` |
| Modify `toolgate/tests/test_handlers_builtin.py` | Интеграционные тесты transcribe |
| Modify `toolgate/tests/test_handlers_summarize_video.py` | Интеграционные тесты summarize_video |
| Modify `ui/src/components/chat/VideoProgressIndicator.tsx`, `ui/src/i18n/locales/ru.json`, `en.json` | Фаза `fix_terms` |

---

### Task 1: `ctx.has_capability` probe

**Files:**
- Modify: `toolgate/handlers/context.py` (dataclass `HandlerContext` ~строка 274, `build_context` ~строка 322)
- Test: `toolgate/tests/test_handlers_context.py`

**Interfaces:**
- Produces: `await ctx.has_capability(capability: str) -> bool` — True когда активный провайдер настроен, БЕЗ вызова provider-метода; False при отсутствии/ошибке резолва.

- [ ] **Step 1: Write the failing tests**

В `toolgate/tests/test_handlers_context.py` уже есть `_FakeRegistry` (init с dict `active`, `aget_active` возвращает `self._active.get(capability)`). Добавить в конец файла:

```python
# ── has_capability probe ─────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_has_capability_true_when_provider_active():
    ctx = build_context(_FakeRegistry({"websearch": object()}), http_client=None)
    assert await ctx.has_capability("websearch") is True


@pytest.mark.asyncio
async def test_has_capability_false_when_provider_absent():
    ctx = build_context(_FakeRegistry({}), http_client=None)
    assert await ctx.has_capability("websearch") is False


@pytest.mark.asyncio
async def test_has_capability_false_when_registry_raises():
    class _BoomRegistry:
        async def aget_active(self, capability):
            raise RuntimeError("boom")

    ctx = build_context(_BoomRegistry(), http_client=None)
    assert await ctx.has_capability("websearch") is False
```

- [ ] **Step 2: Run tests to verify they fail**

Run (из `toolgate/`): `python -m pytest tests/test_handlers_context.py -k has_capability -v`
Expected: FAIL — `AttributeError: 'HandlerContext' object has no attribute 'has_capability'`

- [ ] **Step 3: Implement**

В `toolgate/handlers/context.py`, в dataclass `HandlerContext` добавить поле рядом с другими приватными (`_job_id` и т.п.):

```python
    _registry: object | None = None
```

и метод после `progress()`:

```python
    async def has_capability(self, capability: str) -> bool:
        """True when an active provider for `capability` is configured.

        Resolve-only probe (no provider call) so handlers can skip an optional
        stage cheaply — e.g. term_fixer skips entirely without websearch,
        BEFORE paying for its detect LLM call.
        """
        if self._registry is None:
            return False
        try:
            return await self._registry.aget_active(capability) is not None
        except Exception:
            return False
```

В `build_context(...)` добавить в конструктор `HandlerContext(...)`:

```python
        _registry=registry,
```

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_handlers_context.py -v`
Expected: PASS (все, включая 3 новых)

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/context.py toolgate/tests/test_handlers_context.py
git commit -m "feat(toolgate): ctx.has_capability resolve-only probe"
```

---

### Task 2: term_fixer — каркас, парсер detect-JSON, фильтры кандидатов

**Files:**
- Create: `toolgate/term_fixer.py`
- Create: `toolgate/tests/test_term_fixer.py`

**Interfaces:**
- Produces:
  - `@dataclass Replacement(heard: str, variants: list[str], corrected: str, confidence: str, description: str, matched: bool = False)`
  - `@dataclass FixResult(transcript: str, replacements: list[Replacement], glossary_md: str, term_notes: str)`
  - `parse_detect_json(raw: str) -> list[dict] | None` — толерантный парсер LLM-выхода; `None` = НЕ РАСПАРСИЛОСЬ (сигнал для warning), `[]` = честный «кандидатов нет»
  - `normalize_candidate(item: dict) -> dict | None` — `{"heard","variants","description","query"}` или None
  - константы из Global Constraints

- [ ] **Step 1: Write the failing tests**

Создать `toolgate/tests/test_term_fixer.py`:

```python
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_term_fixer.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'term_fixer'`

- [ ] **Step 3: Implement**

Создать `toolgate/term_fixer.py`:

```python
"""term_fixer — обнаружение и исправление искажённых STT названий.

Пайплайн detect → search → verify → apply; см. спеку
docs/superpowers/specs/2026-07-10-transcript-term-correction-design.md.

Обработчики импортируют модуль лениво внутри run() (`from term_fixer import
fix_terms`) — корень toolgate добавляет в sys.path async-runner
(handlers/runner.py). НЕ использовать parents[3]-шов обработчиков.
"""
from __future__ import annotations

import asyncio
import json
import re
from dataclasses import dataclass, field

MIN_FIX_CHARS = 300
MAX_CANDIDATES = 8
MAX_VARIANTS = 10
MIN_VARIANT_LEN = 3
MAX_CORRECTED_LEN = 80
MAX_QUERY_LEN = 200
DETECT_WINDOW_MIN = 45
DETECT_WINDOW_CHARS = 24_000
SEARCH_CONCURRENCY = 4

# Чисто цифровые / цифро-дефисные словоформы запрещены: variant «37» изувечил
# бы таймкоды [12:37] и любые числа в тексте.
_DIGITS_ONLY_RE = re.compile(r"^[\d\-]+$")

# Allowlist для corrected: unicode-буквы/цифры/_ (\w), пробел и .-&+().
# Markdown-символы #[]!`<> не проходят по построению.
_CORRECTED_ALLOWED_RE = re.compile(r"^[\w .\-&+()]+$")

_FENCE_RE = re.compile(r"^```[a-zA-Z]*\n(.*)\n```$", re.DOTALL)


@dataclass
class Replacement:
    heard: str
    variants: list[str]
    corrected: str
    confidence: str          # "high" | "low"
    description: str         # ТОЛЬКО из detect (из транскрипта)
    matched: bool = False    # хотя бы одна словоформа найдена в тексте


@dataclass
class FixResult:
    transcript: str
    replacements: list[Replacement] = field(default_factory=list)
    glossary_md: str = ""
    term_notes: str = ""


# ── detect-JSON parsing ──────────────────────────────────────────────────────

def parse_detect_json(raw: str) -> list[dict] | None:
    """Толерантный парсер LLM-выхода: заборы → json.loads → {"candidates": []}
    → первый валидный [...]-блок (raw_decode, НЕ жадный regex — хвост вида
    «Готово [я закончил].» не должен ломать захват).

    None = не распарсилось (оркестратор логирует warning); [] = честный
    «кандидатов нет». Возвращает только list[dict]-элементы.
    """
    text = (raw or "").strip()
    m = _FENCE_RE.match(text)
    if m:
        text = m.group(1).strip()
    try:
        data = json.loads(text)
    except (ValueError, TypeError):
        data = None
    if isinstance(data, dict):
        data = data.get("candidates")
    if isinstance(data, list):
        return [x for x in data if isinstance(x, dict)]
    idx = text.find("[")
    if idx != -1:
        try:
            data, _ = json.JSONDecoder().raw_decode(text[idx:])
        except ValueError:
            return None
        if isinstance(data, list):
            return [x for x in data if isinstance(x, dict)]
    return None


# ── candidate normalization ──────────────────────────────────────────────────

def _clean_variants(heard: str, variants: list) -> list[str]:
    out: list[str] = []
    seen: set[str] = set()
    for v in [heard, *variants]:
        if not isinstance(v, str):
            continue
        v = v.strip()
        if len(v) < MIN_VARIANT_LEN or _DIGITS_ONLY_RE.match(v):
            continue
        key = v.casefold()
        if key in seen:
            continue
        seen.add(key)
        out.append(v)
        if len(out) >= MAX_VARIANTS:
            break
    return out


def normalize_candidate(item: dict) -> dict | None:
    """Валидация одного detect-кандидата. None = отброшен."""
    heard = item.get("heard")
    description = item.get("description")
    query = item.get("query")
    variants = item.get("variants")
    if not isinstance(heard, str) or not heard.strip():
        return None
    if not isinstance(description, str) or not isinstance(query, str):
        return None
    query = query.strip()
    if not query or "\n" in query or len(query) > MAX_QUERY_LEN:
        return None
    cleaned = _clean_variants(heard.strip(), variants if isinstance(variants, list) else [])
    if not cleaned:
        return None
    return {
        "heard": heard.strip(),
        "variants": cleaned,
        "description": " ".join(description.split()),
        "query": query,
    }
```

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_term_fixer.py -v`
Expected: PASS (все тесты Task 2)

- [ ] **Step 5: Commit**

```bash
git add toolgate/term_fixer.py toolgate/tests/test_term_fixer.py
git commit -m "feat(toolgate): term_fixer scaffolding — detect-JSON parser + candidate filters"
```

---

### Task 3: term_fixer — однопроходный apply-движок

**Files:**
- Modify: `toolgate/term_fixer.py`
- Test: `toolgate/tests/test_term_fixer.py`

**Interfaces:**
- Consumes: `Replacement` (Task 2)
- Produces: `apply_replacements(text: str, reps: list[Replacement], language: str = "ru") -> str` — мутирует `rep.matched` у найденных; при исключении возвращает исходный `text` целиком (атомарность).

- [ ] **Step 1: Write the failing tests**

Добавить в `toolgate/tests/test_term_fixer.py`:

```python
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_term_fixer.py -k apply -v`
Expected: FAIL — `AttributeError: module 'term_fixer' has no attribute 'apply_replacements'`

- [ ] **Step 3: Implement**

Добавить в `toolgate/term_fixer.py`:

```python
# ── apply (однопроходный, атомарный) ─────────────────────────────────────────

_LOW_ANNOTATION = {"ru": "{found} (вероятно {corr}?)", "en": "{found} (likely {corr}?)"}


def apply_replacements(text: str, reps: list[Replacement], language: str = "ru") -> str:
    """Один re.sub по всем словоформам всех кандидатов сразу.

    Однопроходность гарантирует отсутствие каскадов (замены никогда не
    применяются к уже подставленному тексту). Границы слова (?<!\\w)…(?!\\w)
    защищают «тейповый» от variant «тейп»; IGNORECASE закрывает
    капитализацию начала предложения. Любое исключение → исходный текст
    целиком (атомарность).
    """
    pairs: list[tuple[str, Replacement]] = [
        (v, rep) for rep in reps for v in rep.variants if v
    ]
    if not pairs:
        return text
    try:
        # Длинные словоформы раньше в альтернаторе — «амбассадора» до «амбассадор».
        pairs.sort(key=lambda p: len(p[0]), reverse=True)
        by_variant = {v.casefold(): rep for v, rep in reversed(pairs)}
        pattern = re.compile(
            r"(?<!\w)(" + "|".join(re.escape(v) for v, _ in pairs) + r")(?!\w)",
            re.IGNORECASE,
        )
        tmpl = _LOW_ANNOTATION["ru" if language == "ru" else "en"]
        annotated: set[int] = set()

        def _sub(m: re.Match) -> str:
            found = m.group(1)
            rep = by_variant.get(found.casefold())
            if rep is None:
                return found
            rep.matched = True
            if rep.confidence == "high":
                return rep.corrected
            if id(rep) in annotated:
                return found
            annotated.add(id(rep))
            return tmpl.format(found=found, corr=rep.corrected)

        return pattern.sub(_sub, text)
    except Exception:
        # Атомарный откат: текст возвращается исходным, значит и matched-флаги,
        # выставленные до исключения, обязаны быть сброшены — иначе глоссарий
        # и term_notes заявят замены, которых в тексте нет.
        for rep in reps:
            rep.matched = False
        return text
```

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_term_fixer.py -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add toolgate/term_fixer.py toolgate/tests/test_term_fixer.py
git commit -m "feat(toolgate): term_fixer single-pass word-boundary apply engine"
```

---

### Task 4: term_fixer — глоссарий и term_notes

**Files:**
- Modify: `toolgate/term_fixer.py`
- Test: `toolgate/tests/test_term_fixer.py`

**Interfaces:**
- Consumes: `Replacement` (Task 2; учитывается только `matched=True`)
- Produces:
  - `build_glossary(reps: list[Replacement], language: str = "ru") -> str` — markdown-блок или `""`
  - `build_term_notes(reps: list[Replacement], language: str = "ru") -> str` — сводка-инструкция для digest-промптов или `""`

- [ ] **Step 1: Write the failing tests**

```python
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_term_fixer.py -k "glossary or term_notes" -v`
Expected: FAIL — no attribute `build_glossary`

- [ ] **Step 3: Implement**

```python
# ── glossary / term_notes ────────────────────────────────────────────────────

_MD_ESCAPE_RE = re.compile(r"([*_\[\]()!`#<>])")


def _md_escape(s: str) -> str:
    """Одна строка + экранирование markdown-спецсимволов (значения глоссария —
    из недоверенного транскрипта и недоверенной поисковой выдачи)."""
    return _MD_ESCAPE_RE.sub(r"\\\1", " ".join(s.split()))


def build_glossary(reps: list[Replacement], language: str = "ru") -> str:
    rows = [r for r in reps if r.matched]
    if not rows:
        return ""
    ru = language == "ru"
    lines = ["## Исправленные названия" if ru else "## Corrected names"]
    for r in rows:
        heard, corr, desc = _md_escape(r.heard), _md_escape(r.corrected), _md_escape(r.description)
        if r.confidence == "high":
            line = f"- «{heard}» → **{corr}**" + (f" — {desc}" if desc else "")
        else:
            mark = "вероятно" if ru else "likely"
            note = " (не подтверждено)" if ru else " (unconfirmed)"
            line = f"- «{heard}» → *{mark}* **{corr}**{note}"
        lines.append(line)
    return "\n".join(lines)


def build_term_notes(reps: list[Replacement], language: str = "ru") -> str:
    """Сводка для digest-промптов: inline-пометка low видна только одному
    map-чанку, поэтому конвенцию доносим через system-промпт."""
    rows = [r for r in reps if r.matched]
    if not rows:
        return ""
    ru = language == "ru"
    parts = []
    for r in rows:
        if r.confidence == "high":
            parts.append(
                f'"{r.corrected}" (было "{r.heard}")' if ru
                else f'"{r.corrected}" (was "{r.heard}")'
            )
        else:
            parts.append(
                f'"{r.heard}" вероятно означает "{r.corrected}" (не подтверждено)' if ru
                else f'"{r.heard}" likely means "{r.corrected}" (unconfirmed)'
            )
    if ru:
        return (
            "В транскрипте уже исправлены названия: " + "; ".join(parts) +
            '. Используй исправленные написания; названия с пометкой «вероятно» '
            'упоминай с этой пометкой.'
        )
    return (
        "Product names were already corrected in the transcript: " + "; ".join(parts) +
        '. Use the corrected spellings; keep the "likely" mark for unconfirmed ones.'
    )
```

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_term_fixer.py -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add toolgate/term_fixer.py toolgate/tests/test_term_fixer.py
git commit -m "feat(toolgate): term_fixer glossary + term_notes builders (ru/en, md-escape)"
```

---

### Task 5: term_fixer — санитизация verify-вердиктов

**Files:**
- Modify: `toolgate/term_fixer.py`
- Test: `toolgate/tests/test_term_fixer.py`

**Interfaces:**
- Consumes: `Replacement`, константы, нормализованные кандидаты Task 2
- Produces: `sanitize_verdicts(verdicts: list, candidates: dict[int, dict]) -> list[Replacement]` — `candidates` = `{id: {"heard","variants","description","query"}}`; join строго по `id`; санитизация corrected; `already_correct`/no-op/`null` → кандидат выпадает без Replacement.

- [ ] **Step 1: Write the failing tests**

```python
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_term_fixer.py -k sanitize -v`
Expected: FAIL — no attribute `sanitize_verdicts`

- [ ] **Step 3: Implement**

```python
# ── verify sanitization ──────────────────────────────────────────────────────

def sanitize_verdicts(verdicts, candidates: dict[int, dict]) -> list[Replacement]:
    """Join verify-ответа с кандидатами СТРОГО по id (защита от
    кросс-кандидатной инъекции в батче) + санитизация corrected: сниппеты
    недоверенные, нарушение любого лимита = отброс кандидата, не обрезка."""
    out: list[Replacement] = []
    if not isinstance(verdicts, list):
        return out
    remaining = dict(candidates)
    for v in verdicts:
        if not isinstance(v, dict):
            continue
        cid = v.get("id")
        # type(...) is int, НЕ isinstance: bool — подкласс int, и {"id": true}
        # от недоверенного LLM попал бы в pop(True) == pop(1) — вердикт
        # применился бы к чужому кандидату (кросс-кандидатная путаница).
        cand = remaining.pop(cid, None) if type(cid) is int else None
        if cand is None:
            continue
        if v.get("already_correct") is True:
            continue
        corr = v.get("corrected")
        if not isinstance(corr, str) or "\n" in corr:
            continue  # многострочный corrected = отброс, не склейка (спека)
        corr = corr.strip()
        if (
            not corr
            or len(corr) > MAX_CORRECTED_LEN
            or len(corr) > 3 * len(cand["heard"])
            or not _CORRECTED_ALLOWED_RE.match(corr)
        ):
            continue
        if corr.casefold() == cand["heard"].casefold():
            continue  # no-op → already_correct
        conf = v.get("confidence")
        out.append(Replacement(
            heard=cand["heard"],
            variants=cand["variants"],
            corrected=corr,
            confidence=conf if conf in ("high", "low") else "low",
            description=cand["description"],
        ))
    return out
```

Код выше финален — многострочный `corrected` отбрасывается (`"\n" in corr`), НЕ склеивается в одну строку (спека: отброс, не обрезка/нормализация).

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_term_fixer.py -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add toolgate/term_fixer.py toolgate/tests/test_term_fixer.py
git commit -m "feat(toolgate): term_fixer verify sanitization — id-join, corrected allowlist"
```

---

### Task 6: term_fixer — промпты, окна, оркестратор `fix_terms`

**Files:**
- Modify: `toolgate/term_fixer.py`
- Test: `toolgate/tests/test_term_fixer.py`

**Interfaces:**
- Consumes: всё из Task 2–5; `ctx.has_capability` (Task 1); `split_transcript_by_time`, `transcript_minutes`, `strip_transcript_timecodes` из `handlers.builtin.summarize_video` (импорт внутри функций — обратный module-top импорт создал бы цикл, когда summarize_video начнёт импортировать term_fixer)
- Produces: `async fix_terms(ctx, transcript: str, language: str = "ru", progress_pcts: tuple | None = None) -> FixResult` — обработчики зовут ровно эту сигнатуру; `progress_pcts=(d, s, v)` — pct для `ctx.progress("fix_terms", …)` перед detect/search/verify.

- [ ] **Step 1: Write the failing tests**

```python
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_term_fixer.py -k fix_terms -v`
Expected: FAIL — no attribute `fix_terms`

- [ ] **Step 3: Implement**

Добавить в `toolgate/term_fixer.py`:

```python
# ── prompts ──────────────────────────────────────────────────────────────────

DETECT_SYSTEM_PROMPT = (
    "Ты анализируешь фрагмент авто-транскрипта (speech-to-text). Найди названия "
    "продуктов, брендов, моделей и терминов, которые выглядят как ФОНЕТИЧЕСКОЕ "
    "ИСКАЖЕНИЕ распознавания: несуществующие названия, кириллица там, где "
    "ожидается латинский бренд, странные буквенно-цифровые коды.\n"
    "Для каждого верни JSON-объект с ключами:\n"
    '- "heard": точная форма из текста;\n'
    '- "variants": ВСЕ словоформы этого названия, встречающиеся в тексте;\n'
    '- "description": что этот объект ДЕЛАЕТ, по контексту фрагмента;\n'
    '- "query": поисковый запрос на английском ПО ОПИСАНИЮ И ФУНКЦИИ '
    "(бренд + категория + функция), НЕ по искажённому имени.\n"
    "Ответ — ТОЛЬКО JSON-массив. Если искажений нет — пустой массив []."
)

VERIFY_SYSTEM_PROMPT = (
    "Тебе даны кандидаты — возможно искажённые распознаванием названия — и "
    "результаты веб-поиска по их описанию. Для КАЖДОГО кандидата (по его id) "
    "реши, какое РЕАЛЬНОЕ название имелось в виду.\n"
    "Критерии: фонетическое сходство (тейп≈Tape, амбассадор≈MBassador), общие "
    "цифры/коды (37↔J-37), совпадение бренда И функции с описанием.\n"
    "Верни ТОЛЬКО JSON-массив, для каждого id один из вариантов:\n"
    '- {"id": N, "corrected": "Реальное Название", "confidence": "high"|"low"} — '
    '"high" ТОЛЬКО когда сходятся и фонетика, и функция; сомнение — "low";\n'
    '- {"id": N, "already_correct": true} — услышанное само является реальным '
    "названием;\n"
    '- {"id": N, "corrected": null} — подходящего продукта в выдаче нет.'
)


def _detect_messages(window_text: str) -> list[dict]:
    return [
        {"role": "system", "content": DETECT_SYSTEM_PROMPT},
        {"role": "user", "content": window_text},
    ]


def _verify_messages(candidates: list[dict], results: list[list[dict]]) -> list[dict]:
    parts = []
    for i, cand in enumerate(candidates):
        parts.append(
            f"Кандидат id={i}: услышано «{cand['heard']}», "
            f"описание из контекста: {cand['description']}"
        )
        rows = results[i]
        if rows:
            for r in rows:
                parts.append(f"  - {r['title']} | {r['url']} | {r['content']}")
        else:
            parts.append("  (без результатов поиска)")
    return [
        {"role": "system", "content": VERIFY_SYSTEM_PROMPT},
        {"role": "user", "content": "\n".join(parts)},
    ]


# ── windows ──────────────────────────────────────────────────────────────────

def split_windows(transcript: str) -> list[str]:
    """Окна для detect: 45-мин по таймкодам; fallback — по символам.

    Полный транскрипт длинной лекции НИКОГДА не уходит в один LLM-вызов —
    map-reduce в digest существует именно потому, что он не влезает.
    Импорт внутри функции: module-top импорт summarize_video создал бы цикл,
    когда обработчик импортирует term_fixer.
    """
    from handlers.builtin.summarize_video import (  # noqa: PLC0415
        split_transcript_by_time, transcript_minutes,
    )
    if transcript_minutes(transcript) > DETECT_WINDOW_MIN:
        return [c.text for c in split_transcript_by_time(transcript, DETECT_WINDOW_MIN)]
    if len(transcript) > DETECT_WINDOW_CHARS:
        return [transcript[i:i + DETECT_WINDOW_CHARS]
                for i in range(0, len(transcript), DETECT_WINDOW_CHARS)]
    return [transcript]


def _normalize_search_results(res) -> list[dict]:
    out = []
    if not isinstance(res, list):
        return out
    for r in res:
        if not isinstance(r, dict):
            continue
        title = str(r.get("title") or "")
        content = str(r.get("content") or "")
        if not title and not content:
            continue
        out.append({"title": title, "url": str(r.get("url") or ""), "content": content})
    return out


# ── orchestrator ─────────────────────────────────────────────────────────────

async def fix_terms(
    ctx,
    transcript: str,
    language: str = "ru",
    progress_pcts: tuple | None = None,
) -> FixResult:
    """Detect → search → verify → apply. Fail-soft: любая ошибка → исходник."""
    noop = FixResult(transcript=transcript)

    async def _prog(step: int) -> None:
        if progress_pcts and step < len(progress_pcts):
            await ctx.progress("fix_terms", progress_pcts[step])

    try:
        from handlers.builtin.summarize_video import strip_transcript_timecodes  # noqa: PLC0415
        if len(strip_transcript_timecodes(transcript).strip()) < MIN_FIX_CHARS:
            return noop
        if not await ctx.has_capability("websearch"):
            ctx.log.warning("term_fixer: no active websearch provider, skipping")
            return noop

        # ── detect (по окнам, dedup по heard casefold) ────────────────────
        await _prog(0)
        candidates: list[dict] = []
        seen: set[str] = set()
        for window in split_windows(transcript):
            try:
                raw = await ctx.llm.complete(_detect_messages(window))
            except Exception as exc:
                ctx.log.warning("term_fixer: detect window failed: %s", exc)
                continue
            items = parse_detect_json(raw)
            if items is None:
                # None = не распарсилось (в отличие от честного []) — молчаливая
                # смерть detect на слабой модели должна быть видна в логах
                ctx.log.warning("term_fixer: detect window returned unparseable JSON")
                continue
            for item in items:
                cand = normalize_candidate(item)
                if cand is None or cand["heard"].casefold() in seen:
                    continue
                seen.add(cand["heard"].casefold())
                candidates.append(cand)
        if not candidates:
            return noop
        if len(candidates) > MAX_CANDIDATES:
            ctx.log.warning(
                "term_fixer: %d candidates over cap, dropped: %s",
                len(candidates) - MAX_CANDIDATES,
                [c["heard"] for c in candidates[MAX_CANDIDATES:]],
            )
            candidates = candidates[:MAX_CANDIDATES]

        # Кросс-кандидатный дедуп словоформ: общая словоформа у двух кандидатов
        # дала бы недетерминированный lookup в apply (by_variant — один rep на
        # ключ) — словоформа остаётся у первого кандидата.
        taken: set[str] = set()
        for cand in candidates:
            kept = []
            for v in cand["variants"]:
                key = v.casefold()
                if key not in taken:
                    taken.add(key)
                    kept.append(v)
            cand["variants"] = kept
        candidates = [c for c in candidates if c["variants"]]
        if not candidates:
            return noop

        # ── search (параллельно, semaphore) ──────────────────────────────
        await _prog(1)
        sem = asyncio.Semaphore(SEARCH_CONCURRENCY)

        async def _search_one(cand: dict) -> list[dict]:
            async with sem:
                try:
                    # ctx.search — _CapabilityWrapper (НЕ callable!); метод —
                    # ctx.search.search(...), как ctx.stt.transcribe(...)
                    res = await ctx.search.search(cand["query"], max_results=5)
                except Exception as exc:
                    ctx.log.warning("term_fixer: search failed for %r: %s",
                                    cand["heard"], exc)
                    return []
                return _normalize_search_results(res)

        results = list(await asyncio.gather(*[_search_one(c) for c in candidates]))

        # ── verify (один батч) ────────────────────────────────────────────
        await _prog(2)
        try:
            raw = await ctx.llm.complete(_verify_messages(candidates, results))
        except Exception as exc:
            ctx.log.warning("term_fixer: verify failed: %s", exc)
            return noop
        verdicts = parse_detect_json(raw)
        if verdicts is None:
            ctx.log.warning("term_fixer: verify returned unparseable JSON")
            return noop
        reps = sanitize_verdicts(verdicts, dict(enumerate(candidates)))
        if not reps:
            return noop

        # ── apply ─────────────────────────────────────────────────────────
        fixed = apply_replacements(transcript, reps, language)
        matched = [r for r in reps if r.matched]
        if not matched:
            return noop
        return FixResult(
            transcript=fixed,
            replacements=matched,
            glossary_md=build_glossary(matched, language),
            term_notes=build_term_notes(matched, language),
        )
    except Exception as exc:
        try:
            ctx.log.warning("term_fixer failed: %s", exc)
        except Exception:
            pass
        return noop
```

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_term_fixer.py -v`
Expected: PASS (весь файл)

- [ ] **Step 5: Commit**

```bash
git add toolgate/term_fixer.py toolgate/tests/test_term_fixer.py
git commit -m "feat(toolgate): term_fixer orchestrator — windowed detect, parallel search, batch verify"
```

---

### Task 7: интеграция в transcribe

**Files:**
- Modify: `toolgate/handlers/builtin/transcribe.py`
- Test: `toolgate/tests/test_handlers_builtin.py`

**Interfaces:**
- Consumes: `fix_terms(ctx, text, language, progress_pcts=(60, 70, 80))` (Task 6) — импорт внутри `run()`.
- Produces: выдача `ctx.result.text(text + "\n\n---\n" + glossary)` при непустом глоссарии; valve `fix_terms` в `<config>`; фаза `saving 90` сохранена.

- [ ] **Step 1: Write the failing tests**

Добавить в `toolgate/tests/test_handlers_builtin.py` (в файле уже есть импорты `HandlerRegistry`, `BUILTIN_DIR`, `_load`; MagicMock/AsyncMock импортировать при отсутствии):

```python
# ── transcribe × fix_terms ───────────────────────────────────────────────────

def _transcribe_ctx(config=None):
    from unittest.mock import AsyncMock, MagicMock
    from handlers.context import HandlerResult

    ctx = MagicMock()
    ctx.progress = AsyncMock()
    ctx.stt.transcribe = AsyncMock(return_value="Использую амбассадор для баса.")
    ctx.result.text = MagicMock(
        side_effect=lambda s: HandlerResult(status="ok", summary_text=s)
    )
    ctx.result.failed = MagicMock(
        side_effect=lambda r: HandlerResult(status="failed", reason=r)
    )
    ctx.config = config or {}
    ctx.log = MagicMock()
    return ctx


def _audio_file():
    from handlers.context import HandlerFile
    return HandlerFile(bytes=b"FAKEAUDIO", mime="audio/ogg",
                       filename="voice.ogg", size=9)


@pytest.mark.asyncio
async def test_transcribe_fix_terms_on_appends_glossary_after_ruler(monkeypatch):
    import term_fixer as tf
    from handlers.builtin import transcribe as tr_mod

    async def _fake_fix(ctx, text, language, progress_pcts=None):
        return tf.FixResult(transcript="Использую MBassador для баса.",
                            glossary_md="## Исправленные названия\n- «амбассадор» → **MBassador**")

    monkeypatch.setattr(tf, "fix_terms", _fake_fix)
    ctx = _transcribe_ctx()
    res = await tr_mod.run(ctx, _audio_file(), {})
    assert res.summary_text.startswith("Использую MBassador для баса.")
    assert "\n\n---\n## Исправленные названия" in res.summary_text
    # фаза saving сохранена
    assert ("saving", 90) in [c.args for c in ctx.progress.await_args_list]


@pytest.mark.asyncio
async def test_transcribe_fix_terms_valve_off_skips(monkeypatch):
    import term_fixer as tf
    from handlers.builtin import transcribe as tr_mod

    called = {"n": 0}

    async def _fake_fix(*a, **k):
        called["n"] += 1
        return tf.FixResult(transcript="x")

    monkeypatch.setattr(tf, "fix_terms", _fake_fix)
    ctx = _transcribe_ctx(config={"fix_terms": "false"})
    res = await tr_mod.run(ctx, _audio_file(), {})
    assert called["n"] == 0
    assert res.summary_text == "Использую амбассадор для баса."


def test_transcribe_descriptor_declares_fix_terms_valve():
    lh = _load("transcribe")
    f = next((c for c in lh.descriptor.config if c["name"] == "fix_terms"), None)
    assert f is not None and f["default"] == "true"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_handlers_builtin.py -k fix_terms -v`
Expected: FAIL — глоссария нет в выдаче / поля нет в дескрипторе

- [ ] **Step 3: Implement**

В `toolgate/handlers/builtin/transcribe.py`:

1. В XML-дескриптор, блок `<config>` (после поля `default_language`):

```xml
#     <field name="fix_terms" type="bool" default="true" label="Исправлять названия" description="Определять искажённые STT названия (бренды, плагины, термины) и исправлять их через веб-поиск. Транскрипты короче 300 символов пропускаются."/>
```

2. В docstring модуля дописать: `With fix_terms=on the transcript is post-processed (garbled product names corrected via web search) — not strictly verbatim.`

2b. Обновить `<description>` дескриптора (спека требует и docstring, и description):

```xml
#   <description lang="ru">Речь из аудио/видео в текст (без конспекта; названия могут быть автоисправлены)</description>
#   <description lang="en">Speech from audio/video to text (raw, no summary; product names may be auto-corrected)</description>
```

3. Заменить хвост `run()` (после empty-guard, строки 82–83):

```python
    glossary = ""
    fix_enabled = (
        str(ctx.config.get("fix_terms") or "true").strip().lower()
        not in ("false", "0", "no")
    )
    if fix_enabled:
        try:
            from term_fixer import fix_terms  # runner puts toolgate root on sys.path
            fx = await fix_terms(ctx, text, language, progress_pcts=(60, 70, 80))
            text, glossary = fx.transcript, fx.glossary_md
        except Exception as exc:  # fix_terms сам fail-soft; это страховка импорта
            ctx.log.warning("transcribe: fix_terms unavailable: %s", exc)

    await ctx.progress("saving", 90)
    # Глоссарий отделён --- , чтобы читающий агент не считал его частью речи.
    return ctx.result.text(text + (f"\n\n---\n{glossary}" if glossary else ""))
```

**Важно:** monkeypatch в тестах патчит `term_fixer.fix_terms` — поэтому импорт обязан быть `from term_fixer import fix_terms` ВНУТРИ `run()` (резолв атрибута на момент вызова происходит при импорте — при `from`-импорте внутри функции атрибут берётся из модуля при каждом вызове `run()`, патч работает).

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_handlers_builtin.py -v`
Expected: PASS (все, включая старые)

- [ ] **Step 5: Commit**

```bash
git add toolgate/handlers/builtin/transcribe.py toolgate/tests/test_handlers_builtin.py
git commit -m "feat(toolgate): transcribe — fix_terms valve, glossary after ruler, saving phase kept"
```

---

### Task 8: интеграция в summarize_video

**Files:**
- Modify: `toolgate/handlers/builtin/summarize_video.py`
- Test: `toolgate/tests/test_handlers_summarize_video.py`

**Interfaces:**
- Consumes: `fix_terms(ctx, transcript, language, progress_pcts=(40, 43, 46))` (Task 6)
- Produces:
  - `build_single_pass_messages(transcript, duration=0.0, length="medium", term_notes="")` — term_notes дописывается к system-промпту
  - `build_chunk_messages(chunk, idx, total, term_notes="")` — аналогично
  - `build_reduce_messages(partials, length="medium", term_notes="")` — аналогично
  - `build_note(title, duration, transcript, llm_body, include_transcript=True, glossary="")` — глоссарий после тела, перед транскрипт-блоком

- [ ] **Step 1: Write the failing tests**

Добавить в `toolgate/tests/test_handlers_summarize_video.py`:

```python
# ── fix_terms integration ────────────────────────────────────────────────────

def test_builders_append_term_notes_to_system_prompt():
    notes = "В транскрипте уже исправлены названия: ..."
    single = sv_mod.build_single_pass_messages("текст", term_notes=notes)
    assert notes in single[0]["content"]
    chunk = sv_mod.build_chunk_messages(
        sv_mod.TranscriptChunk(0, 45, "текст"), 0, 2, term_notes=notes
    )
    assert notes in chunk[0]["content"]
    reduce_ = sv_mod.build_reduce_messages(["a", "b"], term_notes=notes)
    assert notes in reduce_[0]["content"]
    # без notes — промпт байт-в-байт прежний
    assert sv_mod.build_single_pass_messages("текст")[0]["content"] == (
        sv_mod.SYSTEM_PROMPT
    )


def test_build_note_places_glossary_before_transcript_block():
    g = "## Исправленные названия\n- «x» → **Y**"
    note = sv_mod.build_note("T", 0.0, "[00:01] речь", "## Резюме\nтело",
                             include_transcript=True, glossary=g)
    gi = note.find("## Исправленные названия")
    ti = note.find("> [!note]- Полный транскрипт")
    assert 0 < gi < ti


def test_build_note_without_glossary_unchanged():
    a = sv_mod.build_note("T", 0.0, "речь", "тело")
    b = sv_mod.build_note("T", 0.0, "речь", "тело", glossary="")
    assert a == b


@pytest.mark.asyncio
async def test_run_passes_fixed_transcript_and_term_notes(monkeypatch):
    import term_fixer as tf

    fixed = "Используем MBassador для суб-баса. " * 20
    notes = "В транскрипте уже исправлены названия: \"MBassador\" (было \"амбассадор\")."
    g = "## Исправленные названия\n- «амбассадор» → **MBassador**"

    async def _fake_fix(ctx, transcript, language, progress_pcts=None):
        return tf.FixResult(transcript=fixed, glossary_md=g, term_notes=notes)

    monkeypatch.setattr(tf, "fix_terms", _fake_fix)
    ctx = _make_ctx()
    ctx.stt.transcribe = AsyncMock(return_value="Используем амбассадор. " * 20)
    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        res = await sv_mod.run(ctx, _video_file(), {})
    # digest получил исправленный транскрипт + term_notes в system-промпте
    sys_prompt = ctx.llm.complete.await_args_list[0].args[0][0]["content"]
    user_prompt = ctx.llm.complete.await_args_list[0].args[0][1]["content"]
    assert notes in sys_prompt
    assert "MBassador" in user_prompt
    # заметка содержит глоссарий
    assert "## Исправленные названия" in res.post_action["content"]


@pytest.mark.asyncio
async def test_run_valve_off_no_fix(monkeypatch):
    import term_fixer as tf
    called = {"n": 0}

    async def _fake_fix(*a, **k):
        called["n"] += 1
        return tf.FixResult(transcript="x")

    monkeypatch.setattr(tf, "fix_terms", _fake_fix)
    ctx = _make_ctx()
    ctx.config = {"fix_terms": "false"}
    with patch.object(sv_mod, "extract_audio_from_file", _fake_extract_audio):
        await sv_mod.run(ctx, _video_file(), {})
    assert called["n"] == 0


def test_summarize_descriptor_declares_fix_terms_valve():
    from pathlib import Path
    from handlers.loader import HandlerRegistry
    reg = HandlerRegistry()
    reg.load_all(str(Path(__file__).resolve().parents[1] / "handlers" / "builtin"), None)
    d = reg.get("summarize_video").descriptor
    f = next((c for c in d.config if c["name"] == "fix_terms"), None)
    assert f is not None and f["default"] == "true"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python -m pytest tests/test_handlers_summarize_video.py -k "term_notes or glossary or fix" -v`
Expected: FAIL — `TypeError: ... unexpected keyword argument 'term_notes'`

- [ ] **Step 3: Implement**

В `toolgate/handlers/builtin/summarize_video.py`:

1. Дескриптор `<config>` — добавить после `summary_length`:

```xml
#     <field name="fix_terms" type="bool" default="true" label="Исправлять названия" description="Определять искажённые STT названия (бренды, плагины, термины) и исправлять их через веб-поиск. Транскрипты короче 300 символов пропускаются."/>
```

2. Prompt-builders — добавить параметр и суффикс (одинаковый приём во всех трёх; `term_notes` уже содержит инструкцию про пометку «вероятно», см. Task 4):

```python
def _notes_suffix(term_notes: str) -> str:
    return f"\n\n{term_notes}" if term_notes else ""
```

- `build_single_pass_messages(transcript, duration=0.0, length="medium", term_notes="")`:
  `"content": SYSTEM_PROMPT + _length_suffix(length) + _notes_suffix(term_notes)`
- `build_chunk_messages(chunk, idx, total, term_notes="")`:
  `"content": CHUNK_SYSTEM_PROMPT + _notes_suffix(term_notes)`
- `build_reduce_messages(partials, length="medium", term_notes="")`:
  `"content": REDUCE_SYSTEM_PROMPT + _length_suffix(length) + _notes_suffix(term_notes)`

3. `build_note(...)` — новый параметр `glossary: str = ""`; после `body.strip()` в списке `lines` (перед `if include_transcript:`):

```python
    if glossary:
        lines.append("")
        lines.append(glossary.strip())
```

4. В `run()` сразу после empty-guard (перед `await ctx.progress("digest", 50)`):

```python
    # ── 2b. term correction (spec 2026-07-10-transcript-term-correction) ────
    term_notes = ""
    glossary = ""
    fix_enabled = (
        str(ctx.config.get("fix_terms") or "true").strip().lower()
        not in ("false", "0", "no")
    )
    if fix_enabled:
        try:
            from term_fixer import fix_terms  # runner puts toolgate root on sys.path
            fx = await fix_terms(ctx, transcript, language, progress_pcts=(40, 43, 46))
            transcript, glossary, term_notes = (
                fx.transcript, fx.glossary_md, fx.term_notes,
            )
        except Exception as exc:
            ctx.log.warning("summarize_video: fix_terms unavailable: %s", exc)
```

5. Прокинуть `term_notes` во все три вызова builders внутри `run()`:
   - `build_chunk_messages(chunk, idx, len(chunks), term_notes=term_notes)` (внутри `_map_chunk`)
   - `build_reduce_messages(list(partials), length=summary_length, term_notes=term_notes)`
   - оба вызова `build_single_pass_messages(transcript, length=summary_length, term_notes=term_notes)`

6. Вызов `build_note(...)` — добавить `glossary=glossary`.

- [ ] **Step 3b: Обезопасить существующие тесты — детерминированный skip fix_terms**

В `_make_ctx` (`test_handlers_summarize_video.py:31-65`) добавить строку рядом с `ctx.progress = AsyncMock()`:

```python
    # fix_terms probe: без этого long-transcript тесты (транскрипт > 300 симв.)
    # выживали бы только через TypeError на `await MagicMock()` → fail-soft —
    # хрупко. False = детерминированный skip этапа коррекции.
    ctx.has_capability = AsyncMock(return_value=False)
```

- [ ] **Step 4: Run tests**

Run: `python -m pytest tests/test_handlers_summarize_video.py -v`
Expected: PASS. Старые тесты: короткие транскрипты отсекаются `MIN_FIX_CHARS` до любых ctx-вызовов; длинные — детерминированно скипаются через `has_capability=False` (Step 3b); без `term_notes` промпты байт-в-байт прежние.

- [ ] **Step 5: Run full toolgate suite**

Run: `python -m pytest tests/ -v`
Expected: PASS полностью

- [ ] **Step 6: Commit**

```bash
git add toolgate/handlers/builtin/summarize_video.py toolgate/tests/test_handlers_summarize_video.py
git commit -m "feat(toolgate): summarize_video — fix_terms valve, term_notes in digest prompts, glossary in note"
```

---

### Task 9: UI — фаза fix_terms

**Files:**
- Modify: `ui/src/components/chat/VideoProgressIndicator.tsx:12-17`
- Modify: `ui/src/i18n/locales/ru.json:1255` (после `chat.video_phase_transcribe`)
- Modify: `ui/src/i18n/locales/en.json` (та же позиция)

**Interfaces:**
- Consumes: фаза `fix_terms` приходит сырым ключом в WS-событии `file_job_progress` (core не меняется).

- [ ] **Step 1: Add locale keys**

В `ui/src/i18n/locales/ru.json` после строки `"chat.video_phase_transcribe": "Транскрибирую…",`:

```json
  "chat.video_phase_fix_terms": "Проверяю названия…",
```

В `ui/src/i18n/locales/en.json` в той же позиции:

```json
  "chat.video_phase_fix_terms": "Verifying names…",
```

- [ ] **Step 2: Add PHASES entry**

В `ui/src/components/chat/VideoProgressIndicator.tsx`, карта `PHASES` — вставить между `transcribe` и `digest`:

```typescript
  fix_terms: { emoji: "🔎", key: "chat.video_phase_fix_terms" },
```

- [ ] **Step 3: Type-check and test**

Run (из `ui/`): `npx tsc --noEmit`
Expected: 0 ошибок. Внимание: `TranslationKey` выводится из `ru.json`, а `en.json`
кастуется `as Translations` — пропуск ключа в **en** tsc НЕ поймает.

Run (из `ui/`): `npm test`
Expected: PASS (vitest one-shot; запуск ТОЛЬКО из `ui/` — из корня сканит фантомы).
Паритет локалей гарантирует именно vitest-тест «ru and en have same keys»
(`src/__tests__/hooks.test.ts`) — если забыть en-ключ, упадёт он.

- [ ] **Step 4: Commit**

```bash
git add ui/src/components/chat/VideoProgressIndicator.tsx ui/src/i18n/locales/ru.json ui/src/i18n/locales/en.json
git commit -m "feat(ui): fix_terms progress phase for file handlers"
```

---

### Task 10: финальная верификация

- [ ] **Step 1: Full toolgate suite**

Run (из `toolgate/`): `python -m pytest tests/ -v`
Expected: PASS полностью, ноль регрессий.

- [ ] **Step 2: UI checks**

Run (из `ui/`): `npx tsc --noEmit && npm test`
Expected: PASS.

- [ ] **Step 3: Смок-чтение дескрипторов**

Run (из `toolgate/`):

```bash
python -c "
from handlers.loader import HandlerRegistry
r = HandlerRegistry(); r.load_all('handlers/builtin', None)
for hid in ('transcribe', 'summarize_video'):
    cfg = {c['name']: c['default'] for c in r.get(hid).descriptor.config}
    assert cfg.get('fix_terms') == 'true', (hid, cfg)
print('descriptors OK')
"
```

Expected: `descriptors OK`

- [ ] **Step 4: Commit (если были правки) и стоп**

Деплой НЕ входит в план (по запросу юзера отдельно): toolgate — scp `term_fixer.py`, `handlers/context.py`, `handlers/builtin/{transcribe,summarize_video}.py` + `POST /api/services/toolgate/restart`; UI — `deploy-ui.sh`. Push — только с явного разрешения.
