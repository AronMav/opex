# toolgate/its/site.py
"""ИТС-specific config. VALUES FILLED FROM Phase 0 spike findings
(docs/.../2026-07-03-its-1c-spike-findings.md). Only this file is site-specific."""

SITE_ITS = {
    "base_url": "https://its.1c.ru",
    "auth_probe_url": "https://its.1c.ru/db/",
    "logged_out": {
        # признак разлогина: подстрока в URL ИЛИ наличие селектора формы
        "url_contains": "login.1c.ru",
        "form_selector": "input[name='login']",   # ← из findings
    },
    "login": {
        "login_selector": "input[name='login']",     # ← из findings
        "password_selector": "input[name='password']",# ← из findings
        "submit_selector": "button[type='submit']",   # ← из findings
        "success_url_contains": "its.1c.ru",
        "kicked_selector": None,   # селектор интерстишла "вошли в другом месте" | None
    },
    "read": {
        # путь (a): если задан print_url_template — берём чистый URL;
        # путь (b): иначе SPA-навигация по full_url_template.
        "print_url_template": None,                    # ← из findings, напр. ".../print?..."
        "full_url_template": "{base}/{ref}",
        "content_selector": "#content",                # ← из findings
        "strip_selectors": ["nav", "header", "footer", ".toc"],  # ← из findings
        "wait_selector": "#content",                   # ← из findings
    },
    "search": {
        "url_template": "{base}/db/search?query={q}",  # ← из findings (глоб/пообъектный)
        "db_scoped": False,                            # ← из findings
        "results_wait": ".search-results",              # ← из findings
        "result": "div.result",                         # ← из findings
        "title": "a.r-link",
        "snippet": "span.r-snip",
        "link": "a.r-link",
    },
    "relogin_cooldown_s": 300,
    "read_cache_ttl_s": 86400,
    "search_cache_ttl_s": 3600,
}
