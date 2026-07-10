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
            if not isinstance(rep.corrected, str):
                raise TypeError(f"corrected must be a string, got {type(rep.corrected).__name__}")
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
