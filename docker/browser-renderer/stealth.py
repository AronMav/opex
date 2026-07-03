"""Generic anti-automation-fingerprint hardening. Site-agnostic."""

STEALTH_INIT_JS = """
Object.defineProperty(navigator, 'webdriver', {get: () => undefined});
Object.defineProperty(navigator, 'languages', {get: () => ['ru-RU', 'ru', 'en-US']});
window.chrome = window.chrome || { runtime: {} };
"""

_UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
       "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")


def stealth_context_kwargs() -> dict:
    return {
        "user_agent": _UA,
        "locale": "ru-RU",
        "viewport": {"width": 1280, "height": 800},
        "extra_http_headers": {"Accept-Language": "ru-RU,ru;q=0.9,en;q=0.8"},
    }
