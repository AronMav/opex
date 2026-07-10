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
