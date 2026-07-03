from stealth import STEALTH_INIT_JS, stealth_context_kwargs

def test_init_js_patches_webdriver():
    assert "navigator" in STEALTH_INIT_JS
    assert "webdriver" in STEALTH_INIT_JS

def test_context_kwargs_ru_locale_and_ua():
    kw = stealth_context_kwargs()
    assert kw["locale"].startswith("ru")
    assert "Chrome/" in kw["user_agent"]
    assert kw["viewport"]["width"] >= 1000
