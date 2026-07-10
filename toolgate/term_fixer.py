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
MAX_TERM_LEN = 120
MAX_DESCRIPTION_LEN = 300
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
    # Fallback: сканируем ВСЕ позиции «[», а не только первую — иначе цитата
    # вида «Кандидат [0] найден: [...]» съедала бы реальный массив: raw_decode
    # успешно парсит [0] → список без dict → ложный «честный пустой», а мусор
    # «[ниже]» до массива давал бы None, хотя валидный блок есть дальше.
    idx = text.find("[")
    scans = 0
    while idx != -1 and scans < 20:
        scans += 1
        try:
            data, _ = json.JSONDecoder().raw_decode(text[idx:])
        except ValueError:
            data = None
        if isinstance(data, list):
            dicts = [x for x in data if isinstance(x, dict)]
            if dicts:
                return dicts
        idx = text.find("[", idx + 1)
    return None


# ── candidate normalization ──────────────────────────────────────────────────

def _clean_variants(heard: str, variants: list) -> list[str]:
    out: list[str] = []
    seen: set[str] = set()
    for v in [heard, *variants]:
        if not isinstance(v, str):
            continue
        # Сплющивание \n обязательно: словоформы из недоверенного detect-выхода
        # уходят в regex-альтернатор и (через heard) в построчные промпты.
        v = " ".join(v.split())
        if (
            len(v) < MIN_VARIANT_LEN
            or len(v) > MAX_TERM_LEN
            or _DIGITS_ONLY_RE.match(v)
        ):
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
    # heard — из недоверенного detect-выхода и уходит сырым в построчный
    # verify-промпт («Кандидат id=N: услышано «{heard}»…») и в term_notes →
    # SYSTEM-промпт digest: \n подделал бы чужую строку-карточку, поэтому
    # сплющиваем и ограничиваем длину так же, как variants.
    heard = " ".join(heard.split())
    if not heard or len(heard) > MAX_TERM_LEN:
        return None
    cleaned = _clean_variants(heard, variants if isinstance(variants, list) else [])
    if not cleaned:
        return None
    return {
        "heard": heard,
        "variants": cleaned,
        "description": " ".join(description.split())[:MAX_DESCRIPTION_LEN],
        "query": query,
    }


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
                # re.IGNORECASE (простое посимвольное сворачивание движка) может
                # заматчить форму, чей str.casefold() НЕ совпадает с ключом
                # словаря (турецкая İ: движок матчит «i», а ключ — «i̇»).
                # Добираем тем же движком, чтобы матч не остался без замены.
                for v, r in pairs:
                    if re.fullmatch(re.escape(v), found, re.IGNORECASE):
                        rep = r
                        break
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
        # 45-мин окно плотной речи легко превышает 24k символов — char-кап
        # применяется и к тайм-окнам, иначе он был бы мёртвым кодом для
        # обычного (таймкодного) STT-выхода.
        return [
            slab
            for c in split_transcript_by_time(transcript, DETECT_WINDOW_MIN)
            for slab in _char_slabs(c.text)
        ]
    return _char_slabs(transcript)


def _char_slabs(text: str) -> list[str]:
    if len(text) <= DETECT_WINDOW_CHARS:
        return [text]
    return [text[i:i + DETECT_WINDOW_CHARS]
            for i in range(0, len(text), DETECT_WINDOW_CHARS)]


def _normalize_search_results(res) -> list[dict]:
    """Недоверенные поисковые сниппеты попадают в однострочный verify-промпт
    (`_verify_messages`), где перевод строки в title/content/url подделал бы
    построчную структуру («Кандидат id=N: …») чужой записью. Сплющиваем всё
    в одну строку и капаем content до 500 символов — иначе один раздутый
    сниппет раздувает verify-вызов без пользы для качества решения."""
    out = []
    if not isinstance(res, list):
        return out
    for r in res:
        if not isinstance(r, dict):
            continue
        # Капы на ВСЕ поля: заявленный докстрингом инвариант «раздутый сниппет
        # не раздувает verify-вызов» держался только для content — раздутый
        # title/url обходил его.
        title = " ".join(str(r.get("title") or "").split())[:200]
        content = " ".join(str(r.get("content") or "").split())[:500]
        url = " ".join(str(r.get("url") or "").split())[:300]
        if not title and not content:
            continue
        out.append({"title": title, "url": url, "content": content})
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
        llm_sem = asyncio.Semaphore(SEARCH_CONCURRENCY)

        async def _detect_one(window: str) -> str | None:
            async with llm_sem:
                try:
                    return await ctx.llm.complete(_detect_messages(window))
                except Exception as exc:
                    ctx.log.warning("term_fixer: detect window failed: %s", exc)
                    return None

        # Окна независимы — параллелим как search. gather сохраняет порядок
        # результатов = порядок окон, поэтому dedup по heard и кап «первые 8»
        # остаются детерминированными (первое окно выигрывает).
        raws = await asyncio.gather(*[_detect_one(w) for w in split_windows(transcript)])
        candidates: list[dict] = []
        seen: set[str] = set()
        for raw in raws:
            if raw is None:
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
        provider_gone = False

        async def _search_one(cand: dict) -> list[dict]:
            nonlocal provider_gone
            async with sem:
                try:
                    # ctx.search — _CapabilityWrapper (НЕ callable!); метод —
                    # ctx.search.search(...), как ctx.stt.transcribe(...)
                    res = await ctx.search.search(cand["query"], max_results=5)
                except RuntimeError as exc:
                    # Гейт has_capability прошли, но registry (TTL 30с) успел
                    # потерять провайдера за минуты detect-фазы — отличаем от
                    # транзиентной ошибки поиска, чтобы не гадать без grounding.
                    if "no active websearch provider" in str(exc):
                        provider_gone = True
                        ctx.log.warning(
                            "term_fixer: websearch provider deactivated mid-run: %s", exc)
                    else:
                        ctx.log.warning("term_fixer: search failed for %r: %s",
                                        cand["heard"], exc)
                    return []
                except Exception as exc:
                    ctx.log.warning("term_fixer: search failed for %r: %s",
                                    cand["heard"], exc)
                    return []
                return _normalize_search_results(res)

        results = list(await asyncio.gather(*[_search_one(c) for c in candidates]))
        if provider_gone and not any(results):
            # Совсем без веб-сверки verify гадал бы вслепую — тот режим, который
            # шаг 0 существует чтобы исключить.
            ctx.log.warning("term_fixer: no search grounding at all, skipping correction")
            return noop

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
