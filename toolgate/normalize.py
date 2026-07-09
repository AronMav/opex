"""Russian text normalization for TTS.

TTS-ONLY BY DESIGN: this module expands numbers, abbreviations, and
transliterates English words to Cyrillic — operations that are DESTRUCTIVE
for embedding/search. Do NOT call normalize_text from the indexing pipeline.

Pipeline: pre_process (fast) -> normalize_via_llm (optional) -> post_process (fast)

Pre-processing (<1ms): emoji, markdown, URLs, numbers->words, abbreviations, symbols
LLM normalization: English->Cyrillic transliteration via configurable LLM
Post-processing (<1ms): strip Latin chars, fix punctuation
"""

import asyncio
import logging
import re
import subprocess
import time
from collections import OrderedDict
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


# ── Latin → Cyrillic transliteration ──
# XTTS in ru-mode silently drops Latin and `post_process` strips it, so English
# words go unspoken. When the optional LLM transliteration step is unavailable
# (text providers are not exported to toolgate's registry), this rule-based pass
# keeps English words audible by rendering them in phonetic Cyrillic.

# Russian names of Latin letters — used to spell out acronyms (GPU → джи пи ю).
_LETTER_NAMES = {
    "a": "эй", "b": "би", "c": "си", "d": "ди", "e": "и", "f": "эф",
    "g": "джи", "h": "эйч", "i": "ай", "j": "джей", "k": "кей", "l": "эл",
    "m": "эм", "n": "эн", "o": "оу", "p": "пи", "q": "кью", "r": "ар",
    "s": "эс", "t": "ти", "u": "ю", "v": "ви", "w": "дабл-ю", "x": "экс",
    "y": "уай", "z": "зет",
}

# Hand-tuned pronunciations for common words/acronyms (highest quality).
_TRANSLIT_DICT = {
    "ai": "эй ай", "api": "эй пи ай", "url": "ю эр эл", "id": "ай ди",
    "ok": "окей", "sql": "эс кью эл", "json": "джейсон", "http": "эйч ти ти пи",
    "https": "эйч ти ти пи эс", "css": "си эс эс", "html": "эйч ти эм эл",
    "cpu": "си пи ю", "gpu": "джи пи ю", "ram": "рам", "ssd": "эс эс ди",
    "usb": "ю эс би", "pdf": "пи ди эф", "ui": "ю ай", "ux": "ю экс",
    "python": "пайтон", "java": "джава", "javascript": "джаваскрипт",
    "github": "гитхаб", "git": "гит", "docker": "докер", "linux": "линукс",
    "windows": "виндоус", "android": "андроид", "google": "гугл",
    "telegram": "телеграм", "openai": "оупен эй ай", "chatgpt": "чат джи пи ти",
    "server": "сервер", "online": "онлайн", "offline": "офлайн",
    "email": "имейл", "internet": "интернет", "hello": "хэллоу", "test": "тест",
    # AI / tech proper nouns — espeak's general G2P mispronounces these brand
    # names, so we pin them by hand.
    "gemini": "джемини", "claude": "клод", "chatgpt": "чат джи пи ти",
    "gpt": "джи пи ти", "qwen": "квен", "siri": "сири", "apple": "эпл",
    "microsoft": "майкрософт", "anthropic": "энтропик", "openai": "оупен эй ай",
    "grok": "грок", "llama": "лама", "mistral": "мистраль", "nvidia": "энвидиа",
    "deepseek": "дипсик", "kimi": "кими", "copilot": "копайлот",
    "iphone": "айфон", "ipad": "айпад", "macos": "макос", "nano": "нано",
}

# Multi-letter phonetic rules for the fallback path (matched longest-first).
_DIGRAPHS = [
    ("sch", "ш"), ("tch", "ч"), ("igh", "ай"),
    ("sh", "ш"), ("ch", "ч"), ("th", "т"), ("ph", "ф"), ("wh", "в"),
    ("ck", "к"), ("qu", "кв"), ("oo", "у"), ("ee", "и"), ("ea", "и"),
    ("oa", "оу"), ("ou", "ау"), ("ow", "ау"), ("ay", "эй"), ("ai", "эй"),
    ("ey", "эй"), ("oy", "ой"), ("oi", "ой"), ("ng", "нг"), ("yu", "ю"),
    ("ya", "я"), ("yo", "ё"),
]

_SINGLES = {
    "a": "а", "b": "б", "c": "к", "d": "д", "e": "е", "f": "ф", "g": "г",
    "h": "х", "i": "и", "j": "дж", "k": "к", "l": "л", "m": "м", "n": "н",
    "o": "о", "p": "п", "q": "к", "r": "р", "s": "с", "t": "т", "u": "у",
    "v": "в", "w": "в", "x": "кс", "y": "и", "z": "з",
}

_TRANSLIT_WORD_RE = re.compile(r"[A-Za-z]+(?:['’-][A-Za-z]+)*")


def _translit_word(word: str) -> str:
    low = word.lower()
    if low in _TRANSLIT_DICT:
        return _TRANSLIT_DICT[low]
    letters = [c for c in word if c.isalpha()]
    # All-caps short token → spell it out letter by letter (acronym).
    if word.isupper() and 1 <= len(letters) <= 5:
        return " ".join(_LETTER_NAMES.get(c.lower(), "") for c in letters).strip()
    # Phonetic fallback: greedy digraphs, then single letters.
    out = []
    i = 0
    n = len(low)
    while i < n:
        for dg, repl in _DIGRAPHS:
            if low.startswith(dg, i):
                out.append(repl)
                i += len(dg)
                break
        else:
            out.append(_SINGLES.get(low[i], ""))
            i += 1
    return "".join(out)


# ── espeak-ng G2P: word → IPA → Cyrillic (handles arbitrary words) ──
# Multi-symbol IPA → Cyrillic (matched longest-first: affricates, diphthongs).
_IPA_SEQS = [
    ("dʒ", "дж"), ("tʃ", "ч"),
    ("aɪ", "ай"), ("aʊ", "ау"), ("ɔɪ", "ой"), ("ʌɪ", "ай"), ("eɪ", "эй"),
    ("oʊ", "оу"), ("əʊ", "оу"), ("ɪə", "иэ"), ("iə", "иэ"),
    ("eə", "эа"), ("ʊə", "уэ"),
]
# Single IPA symbol → Cyrillic.
_IPA_SINGLE = {
    "i": "и", "ɪ": "и", "ᵻ": "и", "ɨ": "и",
    "e": "э", "ɛ": "э", "æ": "э",
    "ʌ": "а", "ɑ": "а", "ɐ": "а", "a": "а",
    "ɒ": "о", "ɔ": "о", "o": "о", "ɵ": "о",
    "u": "у", "ʊ": "у", "ʉ": "у",
    "ə": "а", "ɜ": "э", "ɝ": "эр", "ɚ": "эр",
    "p": "п", "b": "б", "t": "т", "d": "д", "k": "к", "g": "г", "ɡ": "г",
    "f": "ф", "v": "в", "θ": "т", "ð": "з", "s": "с", "z": "з",
    "ʃ": "ш", "ʒ": "ж", "h": "х",
    "m": "м", "n": "н", "ŋ": "нг", "l": "л", "ɫ": "л",
    "r": "р", "ɹ": "р", "ɾ": "р", "w": "в", "ʍ": "в", "j": "й",
    " ": "", "ʔ": "", "-": "",
}

# Cache of word → Cyrillic from espeak (persists across requests).
# F119: bounded FIFO cache — TTS text is agent/user-controlled and can contain
# arbitrarily many unique Latin tokens (hashes, identifiers, code), so an
# unbounded dict grew monotonically until the container hit its mem_limit (OOM/502).
_G2P_CACHE_MAX = 10_000
_G2P_CACHE: "OrderedDict[str, str]" = OrderedDict()


def _ipa_to_cyrillic(ipa: str) -> str:
    """Map an espeak IPA string to phonetic Cyrillic."""
    s = (ipa.replace("ˈ", "").replace("ˌ", "")
            .replace("ː", "").replace("ˑ", "").replace("ˌ", ""))
    out: list[str] = []
    i, n = 0, len(s)
    while i < n:
        for seq, cyr in _IPA_SEQS:
            if s.startswith(seq, i):
                out.append(cyr)
                i += len(seq)
                break
        else:
            out.append(_IPA_SINGLE.get(s[i], ""))
            i += 1
    return "".join(out)


def _espeak_ipa_batch(words: list[str]) -> dict[str, str]:
    """Resolve word → IPA via espeak-ng in one call (newline-separated stdin).

    Best-effort: returns {} if espeak is missing/errors or its line count
    doesn't line up with the input (caller then falls back to rule-based)."""
    if not words:
        return {}
    try:
        proc = subprocess.run(
            ["espeak-ng", "-v", "en-us", "-q", "--ipa"],
            input="\n".join(words),
            capture_output=True, text=True, encoding="utf-8", timeout=10,
        )
    except (FileNotFoundError, OSError, subprocess.SubprocessError):
        return {}
    if proc.returncode != 0:
        return {}
    lines = proc.stdout.split("\n")
    while lines and lines[-1].strip() == "":
        lines.pop()
    lines = [ln.strip() for ln in lines]
    if len(lines) != len(words):
        return {}
    return dict(zip(words, lines))


def transliterate_latin(text: str) -> str:
    """Render Latin words as Cyrillic the TTS can pronounce.

    Per word, in order: curated dictionary → acronym letter-spelling →
    espeak-ng G2P (any word, via pronunciation) → rule-based phonetic fallback
    (when espeak is unavailable)."""
    words = _TRANSLIT_WORD_RE.findall(text)
    if not words:
        return text

    resolved: dict[str, str] = {}
    need_espeak: list[str] = []
    for w in words:
        if w in resolved or w in need_espeak:
            continue
        low = w.lower()
        if low in _TRANSLIT_DICT:
            resolved[w] = _TRANSLIT_DICT[low]
            continue
        letters = [c for c in w if c.isalpha()]
        if w.isupper() and 1 <= len(letters) <= 5:
            resolved[w] = " ".join(_LETTER_NAMES.get(c.lower(), "") for c in letters).strip()
            continue
        if w in _G2P_CACHE:
            resolved[w] = _G2P_CACHE[w]
            continue
        need_espeak.append(w)

    if need_espeak:
        ipa_map = _espeak_ipa_batch(need_espeak)
        for w in need_espeak:
            cyr = _ipa_to_cyrillic(ipa_map[w]) if ipa_map.get(w) else ""
            if not cyr:
                cyr = _translit_word(w)  # rule-based fallback
            _G2P_CACHE[w] = cyr
            if len(_G2P_CACHE) > _G2P_CACHE_MAX:
                _G2P_CACHE.popitem(last=False)  # F119: evict oldest (FIFO)
            resolved[w] = cyr

    return _TRANSLIT_WORD_RE.sub(
        lambda m: resolved.get(m.group(0)) or _translit_word(m.group(0)), text
    )


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
    # Transliterate first so English words survive as Cyrillic; the strips below
    # then only remove any residual Latin (e.g. unmapped symbols).
    text = transliterate_latin(text)
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
    # post_process now shells out to espeak-ng for G2P — run off the event loop.
    text = await asyncio.to_thread(post_process, text)
    return text


