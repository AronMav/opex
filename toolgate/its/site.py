# toolgate/its/site.py
"""ИТС-specific config. VALUES VERIFIED against the live its.1c.ru on 2026-07-03
via an authenticated browser session (Phase 0 spike). Only this file is
site-specific — flows.py stays generic."""

SITE_ITS = {
    "base_url": "https://its.1c.ru",
    # Any real page works as an auth probe; the header carries the logged-in
    # marker. (The old "/db/" 404s and never redirected to the login form.)
    "auth_probe_url": "https://its.1c.ru/",
    "logged_out": {
        # The logout link (href ".../?action=logout...") is rendered ONLY when
        # authenticated. Its absence in the page == logged out. Detection is by
        # page content, NOT by URL — its.1c.ru does not redirect anonymous hits
        # to the login form, it just shows a "Вход" link.
        "logged_in_marker": "action=logout",
    },
    "login": {
        # Multi-step SSO: its.1c.ru/user/auth → "Войти через Портал 1С:ИТС"
        # (#login_portal) → login.1c.ru form (username/password) → submit →
        # redirect back to its.1c.ru.
        "auth_page": "https://its.1c.ru/user/auth?backurl=%2F",
        "portal_selector": "#login_portal",
        "username_selector": "input[name='username']",
        "password_selector": "input[name='password']",
        "submit_selector": "input[name='submit']",
        # After login, the logged-in marker must be present again.
        "success_marker": "action=logout",
    },
    "read": {
        # The ref (e.g. "/db/utovio/content/387/hdoc/...") loads a shell page
        # that renders the actual article inside an iframe. flows.read hops to
        # the iframe's src (a clean /db/content/.../src/*.htm page) and extracts
        # #content from there — the shell page itself only holds header + TOC.
        "print_url_template": None,
        "full_url_template": "{base}{ref}",
        "doc_frame_selector": "#w_metadata_doc_frame",
        "content_selector": "#content",
        "strip_selectors": ["nav", "header", "footer", "script", "style"],
        "wait_selector": "#content",
    },
    "search": {
        # GET form action from the sidebar search box; redirects to
        # /db/morphmerged/search/its/{q} with the results.
        "url_template": "{base}/db/morphmerged/search/all?query={q}",
        "db_scoped": False,
        "results_wait": ".search_results_container",
        # Each result is a .panel inside the results container: a title link
        # (a.search_link) in the heading + an optional .search_preview snippet.
        "result": ".search_results_container .panel",
        "title": "a.search_link",
        "snippet": ".search_preview",
        "link": "a.search_link",
    },
    "relogin_cooldown_s": 300,
    "read_cache_ttl_s": 86400,
    "search_cache_ttl_s": 3600,
}
