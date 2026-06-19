import re

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
    t = _punct_to_pause(t)
    t = _collapse_ws(t)
    return t
