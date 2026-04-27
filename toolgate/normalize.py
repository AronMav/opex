"""Russian text normalization for TTS.

TTS-ONLY BY DESIGN: this module expands numbers, abbreviations, and
transliterates English words to Cyrillic — operations that are DESTRUCTIVE
for embedding/search. Do NOT call normalize_text from the indexing pipeline.

Pipeline: pre_process (fast) -> normalize_via_llm (optional) -> post_process (fast)

Pre-processing (<1ms): emoji, markdown, URLs, numbers->words, abbreviations, symbols
LLM normalization: English->Cyrillic transliteration via configurable LLM
Post-processing (<1ms): strip Latin chars, fix punctuation
"""

import logging
import re
import time
from dataclasses import dataclass

import httpx

log = logging.getLogger("normalize")

# ── Configuration ──


@dataclass(frozen=True)
class NormalizeLLMConfig:
    """Config for LLM-based English→Cyrillic transliteration step.
    Passed in by the caller (e.g. Qwen3TTS) who resolves it from the
    provider registry. None = skip LLM step."""
    base_url: str
    api_key: str
    model: str
    timeout: int = 60

# ── Russian number-to-words ──

_ONES = ["", "один", "два", "три", "четыре", "пять", "шесть", "семь", "восемь", "девять"]
_ONES_F = ["", "одна", "две"]
_TEENS = ["десять", "одиннадцать", "двенадцать", "тринадцать", "четырнадцать",
          "пятнадцать", "шестнадцать", "семнадцать", "восемнадцать", "девятнадцать"]
_TENS = ["", "", "двадцать", "тридцать", "сорок", "пятьдесят",
         "шестьдесят", "семьдесят", "восемьдесят", "девяносто"]
_HUNDREDS = ["", "сто", "двести", "триста", "четыреста", "пятьсот",
             "шестьсот", "семьсот", "восемьсот", "девятьсот"]


def _num_group(n: int, feminine: bool = False) -> str:
    if n == 0:
        return ""
    parts = []
    if n >= 100:
        parts.append(_HUNDREDS[n // 100])
        n %= 100
    if 10 <= n <= 19:
        parts.append(_TEENS[n - 10])
        return " ".join(parts)
    if n >= 10:
        parts.append(_TENS[n // 10])
        n %= 10
    if n > 0:
        if feminine and n <= 2:
            parts.append(_ONES_F[n])
        else:
            parts.append(_ONES[n])
    return " ".join(parts)


def _decline(n: int, one: str, two: str, five: str) -> str:
    n = abs(n) % 100
    if 11 <= n <= 19:
        return five
    n %= 10
    if n == 1:
        return one
    if 2 <= n <= 4:
        return two
    return five


def num_to_words(n: int) -> str:
    if n == 0:
        return "ноль"
    if n < 0:
        return "минус " + num_to_words(-n)
    parts = []
    if n >= 1_000_000_000:
        b = n // 1_000_000_000
        parts.append(_num_group(b) + " " + _decline(b, "миллиард", "миллиарда", "миллиардов"))
        n %= 1_000_000_000
    if n >= 1_000_000:
        m = n // 1_000_000
        parts.append(_num_group(m) + " " + _decline(m, "миллион", "миллиона", "миллионов"))
        n %= 1_000_000
    if n >= 1000:
        t = n // 1000
        parts.append(_num_group(t, feminine=True) + " " + _decline(t, "тысяча", "тысячи", "тысяч"))
        n %= 1000
    remainder = _num_group(n)
    if remainder:
        parts.append(remainder)
    return " ".join(parts).strip()


def _replace_number(match: re.Match) -> str:
    text = match.group(0)
    if "," in text or "." in text:
        sep = "," if "," in text else "."
        parts = text.split(sep)
        if len(parts) == 2:
            try:
                whole = num_to_words(int(parts[0]))
                frac = " ".join(_ONES[int(d)] if int(d) > 0 else "ноль" for d in parts[1])
                return whole + " и " + frac
            except ValueError:
                return text
    try:
        return num_to_words(int(text.replace(" ", "")))
    except ValueError:
        return text


# ── Compiled regex patterns ──

_EMOJI_RE = re.compile(
    "[\U0001F600-\U0001F64F\U0001F300-\U0001F5FF\U0001F680-\U0001F6FF"
    "\U0001F1E0-\U0001F1FF\U00002700-\U000027BF\U0000FE00-\U0000FE0F"
    "\U0000200D\U00002600-\U000026FF\U0001F900-\U0001F9FF"
    "\U0001FA00-\U0001FA6F\U0001FA70-\U0001FAFF]+",
    flags=re.UNICODE,
)

_SYMBOLS = {
    "₽": " рублей", "$": " долларов", "€": " евро",
    "%": " процентов", "№": " номер", "&": " и",
    "°C": " градусов Цельсия", "°": " градусов",
    "~": " примерно ", "+": " плюс ",
}

_ABBREVS = [
    (re.compile(r"\bт\.д\.", re.I), "так далее"),
    (re.compile(r"\bт\.е\.", re.I), "то есть"),
    (re.compile(r"\bт\.п\.", re.I), "тому подобное"),
    (re.compile(r"\bт\.к\.", re.I), "так как"),
    (re.compile(r"\bтыс\.", re.I), "тысяч"),
    (re.compile(r"\bмлн\.?\b", re.I), "миллионов"),
    (re.compile(r"\bмлрд\.?\b", re.I), "миллиардов"),
    (re.compile(r"\bруб\.", re.I), "рублей"),
    (re.compile(r"\bшт\.", re.I), "штук"),
    (re.compile(r"\bкг\b"), "килограмм"),
    (re.compile(r"\bкм\b"), "километров"),
    (re.compile(r"\bмл\b"), "миллилитров"),
    (re.compile(r"\bг\.\s", re.I), "года "),
    (re.compile(r"\bгг\.", re.I), "годов"),
    (re.compile(r"\bн\.э\.", re.I), "нашей эры"),
    (re.compile(r"\bдр\.", re.I), "другое"),
]

_MARKDOWN_RE = [
    (re.compile(r"```[\s\S]*?```"), ""),
    (re.compile(r"`[^`]+`"), ""),
    (re.compile(r"\*\*([^*]+)\*\*"), r"\1"),
    (re.compile(r"\*([^*]+)\*"), r"\1"),
    (re.compile(r"__([^_]+)__"), r"\1"),
    (re.compile(r"_([^_]+)_"), r"\1"),
    (re.compile(r"^#{1,6}\s+", re.M), ""),
    (re.compile(r"\[([^\]]+)\]\([^)]+\)"), r"\1"),
    (re.compile(r"^\|.*\|$", re.M), ""),
    (re.compile(r"^\|[-:| ]+\|$", re.M), ""),
    (re.compile(r"^>\s*", re.M), ""),
    (re.compile(r"^---+$", re.M), ""),
]

_URL_RE = re.compile(r"https?://\S+")
_EMAIL_RE = re.compile(r"\S+@\S+\.\S+")
_LATIN_WORD_RE = re.compile(r"\b[a-zA-Z]{2,}\b")
_LATIN_CHAR_RE = re.compile(r"(?<![a-zA-Zа-яА-ЯёЁ])[a-zA-Z](?![a-zA-Zа-яА-ЯёЁ])")


# ── Pre-processing ──

def pre_process(text: str) -> str:
    text = _EMOJI_RE.sub("", text)
    text = _URL_RE.sub("", text)
    text = _EMAIL_RE.sub("", text)
    for pat, repl in _MARKDOWN_RE:
        text = pat.sub(repl, text)
    for sym, word in _SYMBOLS.items():
        text = text.replace(sym, word)
    for pat, repl in _ABBREVS:
        text = pat.sub(repl, text)
    text = re.sub(r"^\d+\.\s*", "— ", text, flags=re.M)
    text = re.sub(r"^[-*•]\s+", "— ", text, flags=re.M)
    text = re.sub(r"\d+[,.]\d+", _replace_number, text)
    text = re.sub(r"\d+(?:\s\d{3})+", _replace_number, text)
    text = re.sub(r"\d+", _replace_number, text)
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


# ── Post-processing ──

def post_process(text: str) -> str:
    text = _LATIN_WORD_RE.sub("", text)
    text = _LATIN_CHAR_RE.sub("", text)
    lines = text.split("\n")
    for i, line in enumerate(lines):
        s = line.strip()
        if s.startswith("— ") and len(s) > 2 and s[-1] not in ".!?;:,":
            lines[i] = line.rstrip() + "."
    text = "\n".join(lines)
    lines = text.split("\n")
    for i, line in enumerate(lines):
        s = line.strip()
        if s and not s.startswith("—") and len(s) > 10 and s[-1] not in ".!?;:,—":
            lines[i] = line.rstrip() + "."
    text = "\n".join(lines)
    text = re.sub(r" ([,.:;!?])", r"\1", text)
    text = re.sub(r"\(\s*\)", "", text)
    text = re.sub(r"—\s*\.", "", text)
    text = re.sub(r"\.{2,}", ".", text)
    text = re.sub(r",{2,}", ",", text)
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"^\s*$\n?", "", text, flags=re.M)
    return text.strip()


# ── LLM normalization ──

_LLM_SYSTEM_PROMPT = """\
Ты — препроцессор текста для русского синтезатора речи. Верни ТОЛЬКО обработанный текст.

Текст уже очищен от чисел, символов и markdown. Тебе остаётся:

1. Английские слова и аббревиатуры — транслитерируй кириллицей или переведи на русский.
   Примеры: Machine Learning → Машин Лёрнинг, AI → ай ай, GPU → джи пи ю,
   API → эй пи ай, Python → Пайтон, JavaScript → ДжаваСкрипт.
2. Расставь запятые и точки для естественных пауз при чтении вслух.
3. В выводе НЕ ДОЛЖНО быть латинских букв (a-z, A-Z).
4. Сохрани весь смысл текста. НЕ добавляй ничего от себя.
5. НЕ оборачивай ответ в кавычки."""


async def normalize_via_llm(
    http: httpx.AsyncClient,
    text: str,
    config: NormalizeLLMConfig | None,
) -> str | None:
    """Transliterate English words to Cyrillic via LLM.
    Returns None if skipped (no config, no API key, no Latin chars, or error)."""
    if config is None or not config.api_key:
        return None
    if not re.search(r"[a-zA-Z]", text):
        log.info("No Latin chars, skipping LLM")
        return None

    try:
        t0 = time.monotonic()
        resp = await http.post(
            config.base_url,
            headers={"Authorization": f"Bearer {config.api_key}"},
            json={
                "model": config.model,
                "messages": [
                    {"role": "system", "content": _LLM_SYSTEM_PROMPT},
                    {"role": "user", "content": text},
                ],
                "temperature": 0.0,
                "max_tokens": max(len(text) * 3, 400),
                "tools": [],
            },
            timeout=float(config.timeout),
        )
        resp.raise_for_status()
        data = resp.json()
        elapsed = time.monotonic() - t0
        result = data["choices"][0]["message"]["content"].strip()

        if "<think>" in result:
            without = re.sub(r"<think>.*?</think>", "", result, flags=re.DOTALL).strip()
            if without:
                result = without

        for oq, cq in [('"', '"'), ("«", "»"), ("'", "'"), ("`", "`")]:
            if result.startswith(oq) and result.endswith(cq) and len(result) > 2:
                result = result[1:-1].strip()
                break

        if not result or len(result) < len(text) * 0.3:
            log.warning("LLM suspicious result (len %d vs input %d)", len(result), len(text))
            return None

        log.info("LLM OK in %.1fs (%d→%d chars)", elapsed, len(text), len(result))
        return result
    except Exception as e:
        log.warning("LLM failed: %s", e)
        return None


async def normalize_text(
    http: httpx.AsyncClient,
    text: str,
    config: NormalizeLLMConfig | None = None,
) -> str:
    """Full normalization pipeline: pre_process -> LLM transliteration -> post_process.
    If config is None, the LLM step is skipped (pre + post only)."""
    text = pre_process(text)
    if config is not None:
        llm_result = await normalize_via_llm(http, text, config)
        if llm_result is not None:
            text = llm_result
    text = post_process(text)
    return text


