import re
import subprocess

from num2words import num2words

_MONTHS = {
    1: "января", 2: "февраля", 3: "марта", 4: "апреля", 5: "мая", 6: "июня",
    7: "июля", 8: "августа", 9: "сентября", 10: "октября", 11: "ноября", 12: "декабря",
}

_UNITS = {
    "кг": "килограмм", "мг": "миллиграмм", "г": "грамм", "т": "тонн",
    "км": "километр", "см": "сантиметр", "мм": "миллиметр", "м²": "квадратный метр",
    "м³": "кубический метр", "м": "метр",
    "мл": "миллилитр", "л": "литр",
    "квт": "киловатт", "вт": "ватт", "гц": "герц",
    "гб": "гигабайт", "мб": "мегабайт", "кб": "килобайт", "тб": "терабайт",
    "°c": "градус", "°": "градус",
}

_SYMBOLS = {
    "№": " номер ", "&": " и ", "§": " параграф ", "©": " ", "™": " ",
    "=": " равно ", "×": " умножить ", "~": " примерно ",
}

_ABBR = {
    r"\bт\.\s*д\.": "так далее",
    r"\bт\.\s*п\.": "тому подобное",
    r"\bт\.\s*е\.": "то есть",
    r"\bт\.\s*к\.": "так как",
    r"\bи\s+др\.": "и другие",
    r"\bсм\.": "смотри",
    r"\bстр\.": "страница",
    r"\bул\.": "улица",
    r"\bкв\.": "квартира",
    r"\bруб\.": "рублей",
    r"\bкоп\.": "копеек",
    r"\bтыс\.": "тысяч",
    r"\bмлн\b": "миллионов",
    r"\bмлрд\b": "миллиардов",
    r"\bрис\.": "рисунок",
    r"\bтабл\.": "таблица",
    r"\bпроф\.": "профессор",
    r"\bакад\.": "академик",
}


def _plural(n: int, one: str, few: str, many: str) -> str:
    n = abs(n) % 100
    if 11 <= n <= 14:
        return many
    d = n % 10
    if d == 1:
        return one
    if 2 <= d <= 4:
        return few
    return many


def _num_words(n, to="cardinal"):
    return num2words(n, lang="ru", to=to)


def _strip_markdown(t: str) -> str:
    t = re.sub(r"```.*?```", " ", t, flags=re.S)
    t = re.sub(r"`([^`]*)`", r"\1", t)
    t = re.sub(r"!\[[^\]]*\]\([^)]*\)", " ", t)
    t = re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", t)
    t = re.sub(r"[*_]{1,3}([^*_]+)[*_]{1,3}", r"\1", t)
    t = re.sub(r"^\s{0,3}#{1,6}\s*", "", t, flags=re.M)
    t = re.sub(r"^\s{0,3}>\s?", "", t, flags=re.M)
    t = re.sub(r"\|", " ", t)
    return t


def _strip_electronic(t: str) -> str:
    t = re.sub(r"https?://\S+", " ", t)
    t = re.sub(r"www\.\S+", " ", t)
    t = re.sub(r"\b[\w.+-]+@[\w-]+\.[\w.-]+\b", " ", t)
    return t


def _datetime(t: str) -> str:
    def _date(m):
        d, mo, y = int(m.group(1)), int(m.group(2)), int(m.group(3))
        if not (1 <= mo <= 12 and 1 <= d <= 31):
            return m.group(0)
        return f"{_num_words(d, to='ordinal')} {_MONTHS[mo]} {_num_words(y, to='ordinal')} года"
    t = re.sub(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4})\b", _date, t)

    def _time(m):
        h, mi = int(m.group(1)), int(m.group(2))
        if not (0 <= h <= 23 and 0 <= mi <= 59):
            return m.group(0)
        if mi == 0:
            return f"{_num_words(h)} часов"
        mm = _num_words(mi) if mi >= 10 else f"ноль {_num_words(mi)}"
        return f"{_num_words(h)} {mm}"
    t = re.sub(r"\b([01]?\d|2[0-3]):([0-5]\d)\b", _time, t)
    return t


def _money(t: str) -> str:
    def repl(one, few, many):
        def f(m):
            n = int(m.group(1))
            return f"{_num_words(n)} {_plural(n, one, few, many)}"
        return f
    t = re.sub(r"(\d+)\s*(?:₽|руб\.?)", repl("рубль", "рубля", "рублей"), t)
    t = re.sub(r"\$\s*(\d+)", repl("доллар", "доллара", "долларов"), t)
    t = re.sub(r"(\d+)\s*€", repl("евро", "евро", "евро"), t)
    return t


def _measure(t: str) -> str:
    units = sorted(_UNITS.keys(), key=len, reverse=True)
    pat = r"\b(\d+)\s*(" + "|".join(re.escape(u) for u in units) + r")\b"

    def f(m):
        n = int(m.group(1))
        u = m.group(2).lower()
        return f"{_num_words(n)} {_UNITS.get(u, u)}"
    return re.sub(pat, f, t, flags=re.I)


def _percent(t: str) -> str:
    def f(m):
        n = int(m.group(1))
        return f"{_num_words(n)} {_plural(n, 'процент', 'процента', 'процентов')}"
    return re.sub(r"(\d+)\s*%", f, t)


def _numbers(t: str) -> str:
    def _dec(m):
        a, b = m.group(1), m.group(2)
        frac = (_plural(int(b), "десятая", "десятых", "десятых") if len(b) == 1
                else _plural(int(b), "сотая", "сотых", "сотых"))
        whole = _plural(int(a), "целая", "целых", "целых")
        return f"{_num_words(int(a))} {whole} {_num_words(int(b))} {frac}"
    t = re.sub(r"\b(\d+)[.,](\d+)\b", _dec, t)

    def _ord(m):
        return _num_words(int(m.group(1)), to="ordinal")
    t = re.sub(r"\b(\d+)-(?:й|я|е|го|му|х|ю|ой|ом)\b", _ord, t)

    def _range(m):
        return f"от {_num_words(int(m.group(1)))} до {_num_words(int(m.group(2)))}"
    t = re.sub(r"\b(\d+)\s*[–-]\s*(\d+)\b", _range, t)

    def _phone(m):
        digits = re.sub(r"\D", "", m.group(0))
        prefix = "плюс " if m.group(0).strip().startswith("+") else ""
        return " " + prefix + " ".join(_num_words(int(d)) for d in digits) + " "
    t = re.sub(r"\+?\d[\d\s-]{6,}\d", _phone, t)

    def _card(m):
        return _num_words(int(m.group(0)))
    t = re.sub(r"\b\d+\b", _card, t)
    return t


def _symbols(t: str) -> str:
    for sym, word in _SYMBOLS.items():
        t = t.replace(sym, word)
    return t


def _abbreviations(t: str) -> str:
    for pat, word in _ABBR.items():
        t = re.sub(pat, word, t, flags=re.I)
    return t


def _punct_to_pause(t: str) -> str:
    t = t.replace(":", ",").replace(";", ",")
    t = re.sub(r"[«»\"]", "", t)
    t = re.sub(r"[—–]", ", ", t)
    t = re.sub(r"…+", "...", t)
    t = re.sub(r"!{2,}", "!", t)
    t = re.sub(r"\?{2,}", "?", t)
    t = re.sub(r"[()]", ", ", t)
    return t


def _collapse_ws(t: str) -> str:
    t = re.sub(r"\s+", " ", t)
    t = re.sub(r"\s+([,.!?])", r"\1", t)
    t = re.sub(r",\s*,", ",", t)
    return t.strip(" ,")


# ── Latin → Cyrillic transliteration (espeak-ng G2P + curated dict) ──
# Silero v5_1_ru drops / mispronounces Latin. Render English words in phonetic
# Cyrillic so they get spoken. Per word, in order: curated dict → acronym
# letter-spelling → espeak-ng G2P (any word) → rule-based phonetic fallback
# (when espeak-ng is unavailable).

_LETTER_NAMES = {
    "a": "эй", "b": "би", "c": "си", "d": "ди", "e": "и", "f": "эф",
    "g": "джи", "h": "эйч", "i": "ай", "j": "джей", "k": "кей", "l": "эл",
    "m": "эм", "n": "эн", "o": "оу", "p": "пи", "q": "кью", "r": "ар",
    "s": "эс", "t": "ти", "u": "ю", "v": "ви", "w": "дабл-ю", "x": "экс",
    "y": "уай", "z": "зет",
}

# Hand-tuned pronunciations for common words / brand names (highest quality).
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
    "gemini": "джемини", "claude": "клод", "gpt": "джи пи ти", "qwen": "квен",
    "siri": "сири", "apple": "эпл", "microsoft": "майкрософт",
    "anthropic": "энтропик", "grok": "грок", "llama": "лама",
    "mistral": "мистраль", "nvidia": "энвидиа", "deepseek": "дипсик",
    "kimi": "кими", "copilot": "копайлот", "iphone": "айфон", "ipad": "айпад",
    "macos": "макос", "nano": "нано", "silero": "силеро", "hydeclaw": "хайдкло",
    "pull": "пул", "request": "реквест", "issue": "ишью", "feedback": "фидбэк",
    "weekend": "уикенд", "meeting": "митинг", "deploy": "деплой",
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
    if word.isupper() and 1 <= len(letters) <= 5:
        return " ".join(_LETTER_NAMES.get(c.lower(), "") for c in letters).strip()
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


# espeak-ng G2P: word → IPA → Cyrillic (handles arbitrary words).
_IPA_SEQS = [
    ("dʒ", "дж"), ("tʃ", "ч"),
    ("aɪ", "ай"), ("aʊ", "ау"), ("ɔɪ", "ой"), ("ʌɪ", "ай"), ("eɪ", "эй"),
    ("oʊ", "оу"), ("əʊ", "оу"), ("ɪə", "иэ"), ("iə", "иэ"),
    ("eə", "эа"), ("ʊə", "уэ"),
]
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

_G2P_CACHE: dict[str, str] = {}


def _ipa_to_cyrillic(ipa: str) -> str:
    """Map an espeak IPA string to phonetic Cyrillic."""
    s = (ipa.replace("ˈ", "").replace("ˌ", "")
            .replace("ː", "").replace("ˑ", ""))
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
    Best-effort: {} if espeak is missing / errors / line count mismatches."""
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
    """Render Latin words as Cyrillic Silero can pronounce. Per word, in order:
    curated dict → acronym letter-spelling → espeak-ng G2P → rule fallback."""
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
                cyr = _translit_word(w)
            _G2P_CACHE[w] = cyr
            resolved[w] = cyr
    return _TRANSLIT_WORD_RE.sub(
        lambda m: resolved.get(m.group(0)) or _translit_word(m.group(0)), text
    )


def normalize(text: str) -> str:
    if not text:
        return ""
    t = _strip_markdown(text)
    t = _strip_electronic(t)
    t = _datetime(t)
    t = _money(t)
    t = _measure(t)
    t = _percent(t)
    t = _numbers(t)
    t = _symbols(t)
    t = _abbreviations(t)
    t = transliterate_latin(t)
    t = _punct_to_pause(t)
    t = _collapse_ws(t)
    return t
