# ИТС 1С Browser Tool — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Дать агентам OPEX инструмент `its` для поиска (`its_search`) и чтения (`its_read`) документации на its.1c.ru через персистентную залогиненную браузерную сессию, ведущую себя как обычный пользователь.

**Architecture:** Универсальные браузерные примитивы (именованные персистентные профили + stealth) добавляются в `browser-renderer` (Python/Playwright, не знает про 1С). Всё 1С-специфичное (логин, поиск, извлечение, кэш, креды) живёт в новом пакете `toolgate/its/`, который делегирует реальную работу браузера в `browser-renderer` через его generic `/automation` с `profile="its"`. Агентский YAML-tool `its` бьёт в toolgate. Единственная платная сессия сериализуется `asyncio.Lock` в toolgate.

**Tech Stack:** Python 3 (toolgate FastAPI, browser-renderer FastAPI + Playwright 1.52), Rust (core: один internal-эндпоинт для кредов), YAML-tool, Docker Compose.

## Global Constraints

- **rustls-tls only** — никакого OpenSSL нигде (core-часть).
- **browser-renderer остаётся универсальным** — 0 строк про 1С, 0 кредов внутри него. Все новые фичи (`profile`, stealth) — generic.
- **Backward compatibility** — существующие `/extract`, `/screenshot`, `/automation` без `profile`, и YAML-tool `browser` работают без изменений; `create_session()` без `profile` = прежнее поведение (эфемерная сессия, TTL 5 мин).
- **Креды никогда не попадают в git** — только в OPEX secrets vault (`ITS_CREDENTIALS`, scope global). В коде/фикстурах/логах — плейсхолдеры.
- **`profile` НЕ выставляется в агентском YAML-tool `browser`** — только внутренний оркестратор toolgate использует профили.
- **Язык контента** — ru-RU (Accept-Language, извлечение).
- **Site-специфика — только в одном файле** — Phase 0 spike обязателен ПЕРВЫМ; его findings заполняют `SITE_ITS` (Task 2.5) и валидируют путь `its_read` (a/b). Селекторы не фиксируем до spike.
- **Спека:** `docs/superpowers/specs/2026-07-03-its-1c-browser-tool-design.md` (v2). Каждая задача неявно наследует эти ограничения.

---

## Phase 0 — Spike (разведка, обязательно первым, НЕ TDD)

### Task 0.1: Разведочный Playwright-скрипт → findings + фикстуры

Цель — снять неизвестность реальных механик ИТС на живых кредах. Это throwaway-код: **не коммитим в src**, живёт в scratchpad. Результат — findings-док + HTML-фикстуры, которые питают Phase 1/2.

**Files:**
- Create (throwaway, НЕ в git): `<scratchpad>/its_spike.py`
- Create (в git): `docs/superpowers/specs/2026-07-03-its-1c-spike-findings.md`
- Create (в git): `toolgate/tests/fixtures/its/` — снятые HTML-фикстуры (обезличенные, без кук/токенов)

**Interfaces:**
- Produces (для Phase 2): findings-док с полями, которые заполнят `SITE_ITS` в Task 2.5 — см. чек-лист ниже.

- [ ] **Step 1: Написать spike-скрипт**

Креды берём из env (оператор задаёт локально; в файл НЕ пишем):

```python
# <scratchpad>/its_spike.py  — THROWAWAY, не коммитить
import asyncio, os, json
from pathlib import Path
from playwright.async_api import async_playwright

LOGIN = os.environ["ITS_LOGIN"]
PASSWORD = os.environ["ITS_PASSWORD"]
OUT = Path(os.environ.get("ITS_SPIKE_OUT", "./its_fixtures"))
OUT.mkdir(parents=True, exist_ok=True)
REF_URL = "https://its.1c.ru/db/v854doc#bookmark:adm:TI000001410"

async def main():
    async with async_playwright() as pw:
        ctx = await pw.chromium.launch_persistent_context(
            user_data_dir=str(OUT / "profile"),
            headless=False,  # spike: смотрим глазами, ловим капчу/2FA
            locale="ru-RU",
            user_agent=("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
                        "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"),
        )
        page = ctx.pages[0] if ctx.pages else await ctx.new_page()
        net = []
        page.on("request", lambda r: net.append({"m": r.method, "u": r.url}))

        # 1) LOGIN FLOW
        await page.goto("https://its.1c.ru/db/", wait_until="domcontentloaded")
        (OUT / "01_landing.html").write_text(await page.content(), encoding="utf-8")
        print("URL after landing:", page.url)  # ← редирект на login.1c.ru?
        # ВРУЧНУЮ в открытом окне: найти селекторы полей логина/пароля/кнопки,
        # проверить наличие капчи/2FA/чекбокса "запомнить". Записать в findings.
        # Затем автоматизировать (селекторы уточнить руками из DevTools):
        # await page.fill("<login_selector>", LOGIN)
        # await page.fill("<password_selector>", PASSWORD)
        # await page.click("<submit_selector>")
        # await page.wait_for_url("**its.1c.ru**", timeout=30000)

        # 2) READ CONTENT (#bookmark:) — ищем чистый XHR/print-URL
        net.clear()
        await page.goto(REF_URL, wait_until="networkidle")
        (OUT / "02_read_full.html").write_text(await page.content(), encoding="utf-8")
        (OUT / "02_read_network.json").write_text(
            json.dumps([r for r in net if "its.1c.ru" in r["u"]], ensure_ascii=False, indent=2),
            encoding="utf-8")
        # ← в 02_read_network.json ищем XHR к /db/content/... или print-URL

        # 3) SEARCH
        net.clear()
        # ВРУЧНУЮ: найти поле поиска / URL поиска, выполнить запрос, напр. "регламентное задание"
        # await page.goto("https://its.1c.ru/db/search?...")  # уточнить из DevTools
        (OUT / "03_search.html").write_text(await page.content(), encoding="utf-8")
        (OUT / "03_search_network.json").write_text(
            json.dumps([r for r in net if "its.1c.ru" in r["u"]], ensure_ascii=False, indent=2),
            encoding="utf-8")

        input("ENTER чтобы закрыть...")  # держим окно для ручного осмотра
        await ctx.close()

asyncio.run(main())
```

- [ ] **Step 2: Прогнать вручную, снять фикстуры**

```bash
pip install playwright && playwright install chromium
ITS_LOGIN='...' ITS_PASSWORD='...' python <scratchpad>/its_spike.py
```

Осмотреть открытое окно + сохранённые файлы. **Обезличить** фикстуры (вырезать любые токены/куки/персональные данные из HTML) перед копированием в `toolgate/tests/fixtures/its/`.

- [ ] **Step 3: Заполнить findings-док**

`docs/superpowers/specs/2026-07-03-its-1c-spike-findings.md` — обязательные разделы (это вход для Task 2.5):

```markdown
# ИТС Spike Findings (2026-07-03)
## Логин
- URL логина: ...
- Селекторы: login=`...`, password=`...`, submit=`...`
- Капча/2FA: есть/нет (→ нужен ли assisted-login, Task 5.1)
- Признак "разлогинено" (селектор/URL-паттерн): ...
- Интерстишл "вошли в другом месте": есть/нет, селектор кнопки: ...
## Сессия
- Долговечность кук (примерно): ...
- Сколько параллельных сессий разрешено: ...
## Чтение (its_read)
- Путь: (a) чистый XHR/print-URL `...`  ИЛИ  (b) только SPA
- Селектор контейнера контента: `...`
- Селекторы для срезки (nav/toc/header/footer): [...]
## Поиск (its_search)
- Глобальный или пообъектный (нужен db-скоуп?): ...
- URL/endpoint: ...
- Селектор списка результатов: `...`; внутри — title=`...`, snippet=`...`, link=`...`
## Анти-бот
- Хватает headless+stealth или нужен headful+xvfb: ...
- Что триггерит блок: ...
```

- [ ] **Step 4: Commit findings + фикстуры (throwaway-скрипт НЕ коммитим)**

```bash
git add -f docs/superpowers/specs/2026-07-03-its-1c-spike-findings.md
git add toolgate/tests/fixtures/its/
git commit -m "docs(its): Phase 0 spike findings + anonymized HTML fixtures"
```

**GATE:** оператор читает findings. Если капча/2FA — активируем Task 5.1 (assisted-login) раньше. Только после этого фиксируем селекторы в Task 2.5.

---

## Phase 1 — browser-renderer: generic-фичи (персистентные профили + stealth)

### Task 1.1: `ProfileManager` — singleton персистентного контекста на профиль

Логику управления профилями выносим в отдельный тестируемый класс с **инъектируемой** фабрикой контекста, чтобы юнит-тесты не требовали реального Chromium.

**Files:**
- Create: `docker/browser-renderer/profiles.py`
- Test: `docker/browser-renderer/test_profiles.py`

**Interfaces:**
- Produces: `class ProfileManager` с
  - `async def get_context(self, profile: str) -> ctx` — лениво создаёт (через `self._factory(user_data_dir)`) ИЛИ возвращает существующий singleton-контекст на имя профиля; сериализует создание через `asyncio.Lock`.
  - `async def close_all(self) -> None`
  - `def profiles_root(self) -> str` (дефолт `/profiles`, override через env `PROFILES_DIR`).
  - `self._factory: Callable[[str], Awaitable[ctx]]` — инъекция.

- [ ] **Step 1: Написать падающий тест**

```python
# docker/browser-renderer/test_profiles.py
import asyncio
import pytest
from profiles import ProfileManager

class FakeCtx:
    def __init__(self, user_data_dir): self.user_data_dir = user_data_dir; self.closed = False
    async def close(self): self.closed = True

@pytest.mark.asyncio
async def test_same_profile_returns_singleton():
    made = []
    async def factory(udd): c = FakeCtx(udd); made.append(c); return c
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    a = await pm.get_context("its")
    b = await pm.get_context("its")
    assert a is b                      # singleton на профиль
    assert len(made) == 1
    assert a.user_data_dir == "/tmp/profiles/its"

@pytest.mark.asyncio
async def test_different_profiles_isolated():
    async def factory(udd): return FakeCtx(udd)
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    a = await pm.get_context("its")
    b = await pm.get_context("other")
    assert a is not b

@pytest.mark.asyncio
async def test_concurrent_get_creates_one():
    made = []
    async def factory(udd):
        await asyncio.sleep(0.01); c = FakeCtx(udd); made.append(c); return c
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    ctxs = await asyncio.gather(*[pm.get_context("its") for _ in range(5)])
    assert len({id(c) for c in ctxs}) == 1   # гонка не плодит контексты
    assert len(made) == 1

@pytest.mark.asyncio
async def test_close_all():
    async def factory(udd): return FakeCtx(udd)
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    c = await pm.get_context("its")
    await pm.close_all()
    assert c.closed is True
```

- [ ] **Step 2: Прогнать — убедиться, что падает**

Run: `cd docker/browser-renderer && pytest test_profiles.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'profiles'`

- [ ] **Step 3: Реализовать `profiles.py`**

```python
# docker/browser-renderer/profiles.py
"""Generic named persistent browser profiles. Site-agnostic."""
import asyncio
import os


class ProfileManager:
    def __init__(self, factory, root: str | None = None):
        # factory: async (user_data_dir: str) -> BrowserContext
        self._factory = factory
        self._root = root or os.environ.get("PROFILES_DIR", "/profiles")
        self._contexts: dict[str, object] = {}
        self._lock = asyncio.Lock()

    def profiles_root(self) -> str:
        return self._root

    async def get_context(self, profile: str):
        existing = self._contexts.get(profile)
        if existing is not None:
            return existing
        async with self._lock:
            existing = self._contexts.get(profile)   # double-checked под локом
            if existing is not None:
                return existing
            udd = os.path.join(self._root, profile)
            os.makedirs(udd, exist_ok=True)
            ctx = await self._factory(udd)
            self._contexts[profile] = ctx
            return ctx

    async def close_all(self) -> None:
        for ctx in list(self._contexts.values()):
            try:
                await ctx.close()
            except Exception:
                pass
        self._contexts.clear()
```

- [ ] **Step 4: Прогнать — зелёные**

Run: `cd docker/browser-renderer && pytest test_profiles.py -v`
Expected: PASS (4 passed)

- [ ] **Step 5: Commit**

```bash
git add docker/browser-renderer/profiles.py docker/browser-renderer/test_profiles.py
git commit -m "feat(browser-renderer): generic ProfileManager (singleton persistent context per profile)"
```

### Task 1.2: Stealth-опции контекста (generic)

Выносим stealth-конфиг в чистую функцию, тестируем её без Chromium.

**Files:**
- Create: `docker/browser-renderer/stealth.py`
- Test: `docker/browser-renderer/test_stealth.py`

**Interfaces:**
- Produces:
  - `STEALTH_INIT_JS: str` — init-script (патч `navigator.webdriver` и т.п.).
  - `def stealth_context_kwargs() -> dict` — kwargs для `launch_persistent_context`/`new_context` (`user_agent`, `locale`, `viewport`).

- [ ] **Step 1: Падающий тест**

```python
# docker/browser-renderer/test_stealth.py
from stealth import STEALTH_INIT_JS, stealth_context_kwargs

def test_init_js_patches_webdriver():
    assert "navigator" in STEALTH_INIT_JS
    assert "webdriver" in STEALTH_INIT_JS

def test_context_kwargs_ru_locale_and_ua():
    kw = stealth_context_kwargs()
    assert kw["locale"].startswith("ru")
    assert "Chrome/" in kw["user_agent"]
    assert kw["viewport"]["width"] >= 1000
```

- [ ] **Step 2: Прогнать — падает**

Run: `cd docker/browser-renderer && pytest test_stealth.py -v` → FAIL (no module `stealth`)

- [ ] **Step 3: Реализовать `stealth.py`**

```python
# docker/browser-renderer/stealth.py
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
```

- [ ] **Step 4: Прогнать — зелёные**

Run: `cd docker/browser-renderer && pytest test_stealth.py -v` → PASS

- [ ] **Step 5: Commit**

```bash
git add docker/browser-renderer/stealth.py docker/browser-renderer/test_stealth.py
git commit -m "feat(browser-renderer): generic stealth context kwargs + init script"
```

### Task 1.3: Подключить профили + stealth в `app.py` (`create_session(profile=...)`)

**Files:**
- Modify: `docker/browser-renderer/app.py` (lifespan, `AutomationRequest`, `create_session` ветка, shutdown)
- Test: `docker/browser-renderer/test_profile_session.py`

**Interfaces:**
- Consumes: `ProfileManager` (1.1), `STEALTH_INIT_JS`/`stealth_context_kwargs` (1.2).
- Produces: `POST /automation {action:"create_session", profile:"its"}` → страница в персистентном stealth-контексте профиля; без `profile` — прежняя эфемерная страница. `close` профильной сессии закрывает **страницу**, но НЕ персистентный контекст.

- [ ] **Step 1: Падающий тест (через FastAPI TestClient с замоканным Playwright-слоем)**

```python
# docker/browser-renderer/test_profile_session.py
import pytest
from fastapi.testclient import TestClient

@pytest.fixture
def client(monkeypatch):
    import app
    class FakePage:
        def __init__(self): self.url = "about:blank"; self.closed = False
        def on(self, *a, **k): pass
        async def close(self): self.closed = True
        async def add_init_script(self, js): self.init_js = js
    class FakeCtx:
        def __init__(self, udd): self.user_data_dir = udd; self.pages_made = []
        async def new_page(self):
            p = FakePage(); self.pages_made.append(p); return p
        async def add_init_script(self, js): self.init_js = js
        async def close(self): pass
    async def fake_factory(udd): return FakeCtx(udd)
    # Подменяем фабрику профиля и общий browser
    app.profile_manager = app.ProfileManager(factory=fake_factory, root="/tmp/pf")
    class FakeBrowser:
        async def new_page(self, **k): return FakePage()
    app.browser = FakeBrowser()
    return TestClient(app.app)

def test_create_profile_session_uses_persistent_context(client):
    r = client.post("/automation", json={"action": "create_session", "profile": "its"})
    assert r.status_code == 200
    assert r.json()["status"] == "created"

def test_create_session_without_profile_still_works(client):
    r = client.post("/automation", json={"action": "create_session"})
    assert r.status_code == 200
    assert "session_id" in r.json()
```

- [ ] **Step 2: Прогнать — падает**

Run: `cd docker/browser-renderer && pytest test_profile_session.py -v`
Expected: FAIL (нет `app.profile_manager` / ветка `profile` не обрабатывается)

- [ ] **Step 3: Реализовать изменения в `app.py`**

Добавить импорт и глобал (рядом с `browser`):

```python
from profiles import ProfileManager
from stealth import STEALTH_INIT_JS, stealth_context_kwargs

profile_manager: ProfileManager | None = None
```

В `lifespan`, после `browser = await pw_instance.chromium.launch(...)`, инициализировать менеджер с реальной фабрикой:

```python
    global profile_manager
    async def _persistent_factory(user_data_dir: str):
        ctx = await pw_instance.chromium.launch_persistent_context(
            user_data_dir=user_data_dir,
            headless=True,
            args=["--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
                  "--disable-blink-features=AutomationControlled"],
            **stealth_context_kwargs(),
        )
        await ctx.add_init_script(STEALTH_INIT_JS)
        return ctx
    profile_manager = ProfileManager(factory=_persistent_factory)
```

В shutdown-части `lifespan` (перед `await browser.close()`):

```python
    if profile_manager:
        await profile_manager.close_all()
```

Добавить поле `profile` в `AutomationRequest`:

```python
    profile: str | None = None
```

Переписать ветку `create_session` в `automation()`:

```python
    if action == "create_session":
        sid = str(uuid.uuid4())[:8]
        if req.profile:
            ctx = await profile_manager.get_context(req.profile)
            page = await ctx.new_page()
        else:
            page = await browser.new_page(
                viewport=DEFAULT_VIEWPORT, user_agent=DEFAULT_USER_AGENT,
            )
        sessions[sid] = page
        page.on("dialog", _make_dialog_handler(sid))
        session_dialog[sid] = {"accept": True, "prompt_text": None, "last": None}
        touch_session(sid)
        return {"session_id": sid, "status": "created", "profile": req.profile}
```

> `close` уже только `page.close()` + pop локального состояния — персистентный контекст НЕ трогается. Это корректно; менять не нужно.

- [ ] **Step 4: Прогнать — зелёные + регресс существующих тестов**

Run: `cd docker/browser-renderer && pytest -v`
Expected: PASS (новые + `test_dispatch.py` без регрессий)

- [ ] **Step 5: Commit**

```bash
git add docker/browser-renderer/app.py docker/browser-renderer/test_profile_session.py
git commit -m "feat(browser-renderer): create_session(profile) → persistent stealth context; ephemeral unchanged"
```

### Task 1.4: Том `/profiles` в docker-compose (персист между пересборками)

**Files:**
- Modify: `docker/docker-compose.yml` (блок `browser-renderer`, строки 33-49; + секция `volumes:`)

**Interfaces:**
- Produces: named volume `browser_profiles` → `/profiles` в контейнере browser-renderer.

- [ ] **Step 1: Добавить том в сервис browser-renderer**

В `docker/docker-compose.yml`, в блоке `browser-renderer:` в список `volumes:` (после строки 42) добавить:

```yaml
      - browser_profiles:/profiles
```

- [ ] **Step 2: Объявить named volume**

В корневую секцию `volumes:` файла (создать, если нет) добавить:

```yaml
volumes:
  browser_profiles:
```

- [ ] **Step 3: Проверить синтаксис compose**

Run: `cd docker && docker compose config >/dev/null && echo OK`
Expected: `OK` (без ошибок парсинга)

- [ ] **Step 4: Commit**

```bash
git add docker/docker-compose.yml
git commit -m "chore(browser-renderer): mount browser_profiles volume at /profiles"
```

---

## Phase 2 — toolgate: ИТС-оркестратор (`toolgate/its/`)

### Task 2.1: `its/extract.py` — извлечение HTML → markdown (чистые функции, TDD)

**Files:**
- Create: `toolgate/its/__init__.py` (пустой)
- Create: `toolgate/its/extract.py`
- Test: `toolgate/tests/its/__init__.py` (пустой), `toolgate/tests/its/test_extract.py`
- Modify: `toolgate/requirements.txt` (+ `markdownify`, `beautifulsoup4`)

**Interfaces:**
- Produces:
  - `def extract_content(html: str, content_selector: str, strip_selectors: list[str]) -> dict` → `{"title": str, "markdown": str, "images_omitted": int}`. Картинки заменяются на `[изображение: <alt>]`, счётчик в `images_omitted`.
  - `def parse_search_results(html: str, cfg: dict) -> list[dict]` → `[{"title","snippet","ref","db"}]`. `cfg` = селекторы из `SITE_ITS["search"]`.

- [ ] **Step 1: Добавить зависимости**

В `toolgate/requirements.txt` дописать:

```
markdownify==0.13.1
beautifulsoup4==4.12.3
```

Установить локально для тестов: `pip install markdownify beautifulsoup4`

- [ ] **Step 2: Падающий тест**

```python
# toolgate/tests/its/test_extract.py
from its.extract import extract_content, parse_search_results

CONTENT_HTML = """
<html><head><title>Регламентные задания</title></head><body>
  <nav class="toc">меню</nav>
  <header>шапка</header>
  <div id="content">
    <h1>Регламентные задания</h1>
    <p>Первый абзац.</p>
    <img src="/x.png" alt="схема">
    <table><tr><th>A</th></tr><tr><td>1</td></tr></table>
  </div>
  <footer>подвал</footer>
</body></html>
"""

def test_extract_strips_nav_and_keeps_content():
    r = extract_content(CONTENT_HTML, content_selector="#content",
                        strip_selectors=["nav", "header", "footer"])
    assert "Регламентные задания" in r["markdown"]
    assert "Первый абзац" in r["markdown"]
    assert "меню" not in r["markdown"]
    assert "подвал" not in r["markdown"]

def test_extract_image_placeholder():
    r = extract_content(CONTENT_HTML, content_selector="#content",
                        strip_selectors=["nav", "header", "footer"])
    assert "[изображение: схема]" in r["markdown"]
    assert r["images_omitted"] == 1

def test_extract_title():
    r = extract_content(CONTENT_HTML, content_selector="#content", strip_selectors=[])
    assert r["title"] == "Регламентные задания"

SEARCH_HTML = """
<div class="search-results">
  <div class="result"><a class="r-link" href="/db/v854doc#bookmark:adm:TI1">Тема 1</a>
    <span class="r-snip">описание 1</span></div>
  <div class="result"><a class="r-link" href="/db/v854doc#bookmark:adm:TI2">Тема 2</a>
    <span class="r-snip">описание 2</span></div>
</div>
"""
SEARCH_CFG = {"result": "div.result", "title": "a.r-link", "snippet": "span.r-snip", "link": "a.r-link"}

def test_parse_search_results():
    rows = parse_search_results(SEARCH_HTML, SEARCH_CFG)
    assert len(rows) == 2
    assert rows[0]["title"] == "Тема 1"
    assert rows[0]["snippet"] == "описание 1"
    assert rows[0]["ref"] == "/db/v854doc#bookmark:adm:TI1"
    assert rows[0]["db"] == "v854doc"
```

- [ ] **Step 3: Прогнать — падает**

Run: `cd toolgate && python -m pytest tests/its/test_extract.py -v` → FAIL (no module `its.extract`)

- [ ] **Step 4: Реализовать `its/extract.py`**

```python
# toolgate/its/extract.py
"""HTML → markdown extraction for ИТС pages. Pure, selector-driven, testable."""
import re
from bs4 import BeautifulSoup
from markdownify import markdownify as md


def extract_content(html: str, content_selector: str, strip_selectors: list[str]) -> dict:
    soup = BeautifulSoup(html, "html.parser")
    title = (soup.title.string.strip() if soup.title and soup.title.string else "")

    root = soup.select_one(content_selector) or soup.body or soup
    for sel in strip_selectors:
        for el in root.select(sel):
            el.decompose()

    images_omitted = 0
    for img in root.find_all("img"):
        alt = img.get("alt", "").strip()
        images_omitted += 1
        img.replace_with(f"[изображение: {alt}]")

    markdown = md(str(root), heading_style="ATX", strip=["script", "style"])
    markdown = re.sub(r"\n{3,}", "\n\n", markdown).strip()
    return {"title": title, "markdown": markdown, "images_omitted": images_omitted}


def parse_search_results(html: str, cfg: dict) -> list[dict]:
    soup = BeautifulSoup(html, "html.parser")
    rows: list[dict] = []
    for node in soup.select(cfg["result"]):
        t = node.select_one(cfg["title"])
        s = node.select_one(cfg["snippet"])
        link = node.select_one(cfg["link"])
        ref = (link.get("href") if link else "") or ""
        m = re.search(r"/db/([^/#?]+)", ref)
        rows.append({
            "title": (t.get_text(strip=True) if t else ""),
            "snippet": (s.get_text(strip=True) if s else ""),
            "ref": ref,
            "db": (m.group(1) if m else ""),
        })
    return rows
```

- [ ] **Step 5: Прогнать — зелёные**

Run: `cd toolgate && python -m pytest tests/its/test_extract.py -v` → PASS

- [ ] **Step 6: Commit**

```bash
git add toolgate/its/__init__.py toolgate/its/extract.py toolgate/tests/its/ toolgate/requirements.txt
git commit -m "feat(toolgate/its): HTML→markdown extraction + search-results parser (pure, TDD)"
```

### Task 2.2: `its/cache.py` — TTL-кэш

**Files:**
- Create: `toolgate/its/cache.py`
- Test: `toolgate/tests/its/test_cache.py`

**Interfaces:**
- Produces: `class TTLCache` с `get(key)->val|None`, `set(key, val, ttl_s)`, инъекция часов `now_fn` для тестов.

- [ ] **Step 1: Падающий тест**

```python
# toolgate/tests/its/test_cache.py
from its.cache import TTLCache

def test_get_before_and_after_expiry():
    t = {"v": 1000.0}
    c = TTLCache(now_fn=lambda: t["v"])
    c.set("k", "val", ttl_s=60)
    assert c.get("k") == "val"
    t["v"] = 1000.0 + 61
    assert c.get("k") is None   # протух

def test_missing_key():
    c = TTLCache(now_fn=lambda: 0.0)
    assert c.get("nope") is None
```

- [ ] **Step 2: Прогнать — падает** → `cd toolgate && python -m pytest tests/its/test_cache.py -v` → FAIL

- [ ] **Step 3: Реализовать `its/cache.py`**

```python
# toolgate/its/cache.py
"""Tiny TTL cache for ИТС read/search results."""
import time


class TTLCache:
    def __init__(self, now_fn=time.monotonic, max_items: int = 512):
        self._now = now_fn
        self._max = max_items
        self._data: dict[str, tuple[float, object]] = {}

    def get(self, key: str):
        item = self._data.get(key)
        if item is None:
            return None
        expires_at, val = item
        if self._now() >= expires_at:
            self._data.pop(key, None)
            return None
        return val

    def set(self, key: str, val, ttl_s: float) -> None:
        if len(self._data) >= self._max:
            self._data.pop(next(iter(self._data)), None)  # простая эвикция FIFO
        self._data[key] = (self._now() + ttl_s, val)
```

- [ ] **Step 4: Прогнать — зелёные** → PASS

- [ ] **Step 5: Commit**

```bash
git add toolgate/its/cache.py toolgate/tests/its/test_cache.py
git commit -m "feat(toolgate/its): TTL cache with injectable clock"
```

### Task 2.3: `its/driver.py` — generic-клиент browser-renderer

Тонкая обёртка над `/automation` с `profile="its"`. Site-agnostic: знает только про примитивы браузера, не про 1С.

**Files:**
- Create: `toolgate/its/driver.py`
- Test: `toolgate/tests/its/test_driver.py`

**Interfaces:**
- Consumes: `httpx.AsyncClient` (инъекция).
- Produces: `class BrowserDriver`:
  - `async def ensure_session() -> str` (ленивое `create_session{profile:"its"}`, кэширует sid)
  - `async def navigate(url) -> dict`, `async def fill(selector, value)`, `async def click(selector)`, `async def wait(selector, timeout=...)`, `async def content() -> dict` (`{html,text,url}`), `async def current_url() -> str`
  - `PROFILE = "its"`, `BROWSER_URL = "http://browser-renderer:9020"`

- [ ] **Step 1: Падающий тест (мок httpx)**

```python
# toolgate/tests/its/test_driver.py
import pytest
from its.driver import BrowserDriver

class FakeResp:
    def __init__(self, payload): self._p = payload
    def raise_for_status(self): pass
    def json(self): return self._p

class FakeHTTP:
    def __init__(self): self.calls = []
    async def post(self, url, json=None, timeout=None):
        self.calls.append(json)
        action = json["action"]
        if action == "create_session": return FakeResp({"session_id": "abc", "status": "created"})
        if action == "content": return FakeResp({"html": "<b>hi</b>", "text": "hi", "url": "u"})
        return FakeResp({"status": "ok"})

@pytest.mark.asyncio
async def test_ensure_session_creates_once_with_profile():
    http = FakeHTTP()
    d = BrowserDriver(http)
    sid1 = await d.ensure_session()
    sid2 = await d.ensure_session()
    assert sid1 == sid2 == "abc"
    creates = [c for c in http.calls if c["action"] == "create_session"]
    assert len(creates) == 1
    assert creates[0]["profile"] == "its"

@pytest.mark.asyncio
async def test_navigate_passes_session_id():
    http = FakeHTTP()
    d = BrowserDriver(http)
    await d.navigate("https://its.1c.ru/db/")
    nav = [c for c in http.calls if c["action"] == "navigate"][0]
    assert nav["url"] == "https://its.1c.ru/db/"
    assert nav["session_id"] == "abc"
```

- [ ] **Step 2: Прогнать — падает** → FAIL

- [ ] **Step 3: Реализовать `its/driver.py`**

```python
# toolgate/its/driver.py
"""Generic browser-renderer client bound to the 'its' persistent profile."""

BROWSER_URL = "http://browser-renderer:9020"
PROFILE = "its"


class BrowserDriver:
    def __init__(self, http, browser_url: str = BROWSER_URL):
        self._http = http
        self._url = browser_url
        self._sid: str | None = None

    async def _call(self, payload: dict, timeout: float = 30.0) -> dict:
        resp = await self._http.post(f"{self._url}/automation", json=payload, timeout=timeout)
        resp.raise_for_status()
        return resp.json()

    async def ensure_session(self) -> str:
        if self._sid:
            return self._sid
        r = await self._call({"action": "create_session", "profile": PROFILE})
        self._sid = r["session_id"]
        return self._sid

    async def reset_session(self) -> None:
        self._sid = None

    async def navigate(self, url: str, timeout: int = 30) -> dict:
        sid = await self.ensure_session()
        return await self._call(
            {"action": "navigate", "session_id": sid, "url": url, "timeout": timeout},
            timeout=timeout + 10)

    async def fill(self, selector: str, value: str) -> dict:
        sid = await self.ensure_session()
        return await self._call({"action": "type", "session_id": sid, "selector": selector, "text": value})

    async def click(self, selector: str) -> dict:
        sid = await self.ensure_session()
        return await self._call({"action": "click", "session_id": sid, "selector": selector})

    async def wait(self, selector: str, timeout: int = 10) -> dict:
        sid = await self.ensure_session()
        return await self._call(
            {"action": "wait", "session_id": sid, "selector": selector, "timeout": timeout},
            timeout=timeout + 10)

    async def content(self) -> dict:
        sid = await self.ensure_session()
        return await self._call({"action": "content", "session_id": sid})

    async def current_url(self) -> str:
        r = await self.content()
        return r.get("url", "")
```

- [ ] **Step 4: Прогнать — зелёные** → PASS

- [ ] **Step 5: Commit**

```bash
git add toolgate/its/driver.py toolgate/tests/its/test_driver.py
git commit -m "feat(toolgate/its): generic BrowserDriver bound to 'its' profile"
```

### Task 2.4: `its/creds.py` — получение кредов от core (кэш)

**Files:**
- Create: `toolgate/its/creds.py`
- Test: `toolgate/tests/its/test_creds.py`

**Interfaces:**
- Consumes: `httpx.AsyncClient`, `CORE_API_URL` + токен (env).
- Produces: `async def get_credentials(http) -> dict|None` → `{"login","password"}` из core `GET /api/internal/its-credentials`; кэширует в модульном синглтоне; `None` если core вернул 404/недоступен.

- [ ] **Step 1: Падающий тест**

```python
# toolgate/tests/its/test_creds.py
import pytest
import its.creds as creds

class FakeResp:
    def __init__(self, code, payload=None): self.status_code = code; self._p = payload
    def json(self): return self._p

class FakeHTTP:
    def __init__(self, resp): self._resp = resp; self.calls = 0
    async def get(self, url, headers=None, timeout=None):
        self.calls += 1; return self._resp

@pytest.mark.asyncio
async def test_get_credentials_caches(monkeypatch):
    creds._CACHE = None
    http = FakeHTTP(FakeResp(200, {"login": "u", "password": "p"}))
    a = await creds.get_credentials(http)
    b = await creds.get_credentials(http)
    assert a == {"login": "u", "password": "p"}
    assert b == a
    assert http.calls == 1   # второй раз из кэша

@pytest.mark.asyncio
async def test_get_credentials_none_on_404():
    creds._CACHE = None
    http = FakeHTTP(FakeResp(404))
    assert await creds.get_credentials(http) is None
```

- [ ] **Step 2: Прогнать — падает** → FAIL

- [ ] **Step 3: Реализовать `its/creds.py`**

```python
# toolgate/its/creds.py
"""Fetch ИТС credentials from Core vault via internal endpoint. Cached."""
import os

CORE_API_URL = os.environ.get("CORE_API_URL", "http://127.0.0.1:18789")
_CACHE: dict | None = None


async def get_credentials(http) -> dict | None:
    global _CACHE
    if _CACHE is not None:
        return _CACHE
    token = os.environ.get("OPEX_AUTH_TOKEN", os.environ.get("AUTH_TOKEN", ""))
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    try:
        resp = await http.get(f"{CORE_API_URL}/api/internal/its-credentials",
                              headers=headers, timeout=5.0)
    except Exception:
        return None
    if resp.status_code != 200:
        return None
    data = resp.json()
    if not data.get("login") or not data.get("password"):
        return None
    _CACHE = {"login": data["login"], "password": data["password"]}
    return _CACHE
```

- [ ] **Step 4: Прогнать — зелёные** → PASS

- [ ] **Step 5: Commit**

```bash
git add toolgate/its/creds.py toolgate/tests/its/test_creds.py
git commit -m "feat(toolgate/its): fetch ITS credentials from core internal endpoint (cached)"
```

### Task 2.5: `its/site.py` (конфиг из spike) + `its/flows.py` (логин/поиск/чтение)

**Files:**
- Create: `toolgate/its/site.py` — `SITE_ITS` dict, значения из Phase 0 findings.
- Create: `toolgate/its/flows.py`
- Test: `toolgate/tests/its/test_flows.py`

**Interfaces:**
- Consumes: `BrowserDriver` (2.3), `extract_content`/`parse_search_results` (2.1), `SITE_ITS`.
- Produces: `class ItsFlows`:
  - `async def ensure_logged_in(creds: dict) -> None` (детект → логин; **cooldown** перелогина; при повторном выкидывании → `raise ItsBusy`)
  - `async def search(query: str, db: str | None) -> list[dict]`
  - `async def read(ref: str) -> dict` (`{title, markdown, url, images_omitted}`; путь a/b по `SITE_ITS`)
  - Исключения: `class ItsBusy(Exception)`, `class ItsLoginFailed(Exception)`.

- [ ] **Step 1: Создать `its/site.py` (значения — из findings §0; ниже структура + пример)**

```python
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
        "results_wait": ".search-results",             # ← из findings
        "result": "div.result",                        # ← из findings
        "title": "a.r-link",
        "snippet": "span.r-snip",
        "link": "a.r-link",
    },
    "relogin_cooldown_s": 300,
    "read_cache_ttl_s": 86400,
    "search_cache_ttl_s": 3600,
}
```

- [ ] **Step 2: Падающий тест (мок драйвера)**

```python
# toolgate/tests/its/test_flows.py
import pytest
from its.flows import ItsFlows, ItsBusy

class FakeDriver:
    def __init__(self, url_seq, content_html=""):
        self._urls = list(url_seq); self._content = content_html
        self.filled = {}; self.clicked = []; self.navigated = []
    async def navigate(self, url, timeout=30): self.navigated.append(url); return {}
    async def fill(self, sel, val): self.filled[sel] = val
    async def click(self, sel): self.clicked.append(sel)
    async def wait(self, sel, timeout=10): return {}
    async def current_url(self):
        return self._urls.pop(0) if len(self._urls) > 1 else self._urls[0]
    async def content(self): return {"html": self._content, "text": "", "url": "u"}
    async def reset_session(self): pass

CFG = {  # минимальный SITE_ITS для теста
    "base_url": "https://its.1c.ru", "auth_probe_url": "https://its.1c.ru/db/",
    "logged_out": {"url_contains": "login.1c.ru", "form_selector": "input#l"},
    "login": {"login_selector": "input#l", "password_selector": "input#p",
              "submit_selector": "button#s", "success_url_contains": "its.1c.ru",
              "kicked_selector": None},
    "relogin_cooldown_s": 300,
}

@pytest.mark.asyncio
async def test_login_performed_when_logged_out():
    # 1-й current_url → на login; после submit → its.1c.ru
    drv = FakeDriver(url_seq=["https://login.1c.ru/", "https://its.1c.ru/db/"])
    clock = {"t": 0.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"])
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled["input#l"] == "u"
    assert drv.filled["input#p"] == "p"
    assert "button#s" in drv.clicked

@pytest.mark.asyncio
async def test_already_logged_in_skips_login():
    drv = FakeDriver(url_seq=["https://its.1c.ru/db/"])
    f = ItsFlows(drv, CFG, now_fn=lambda: 0.0)
    await f.ensure_logged_in({"login": "u", "password": "p"})
    assert drv.filled == {}   # логин не выполнялся

@pytest.mark.asyncio
async def test_relogin_cooldown_raises_busy():
    # всё время на login-странице (выкидывает), в пределах cooldown → ItsBusy
    drv = FakeDriver(url_seq=["https://login.1c.ru/"])
    clock = {"t": 100.0}
    f = ItsFlows(drv, CFG, now_fn=lambda: clock["t"])
    f._last_login_at = 99.0   # только что логинились
    with pytest.raises(ItsBusy):
        await f.ensure_logged_in({"login": "u", "password": "p"})
```

- [ ] **Step 3: Прогнать — падает** → FAIL

- [ ] **Step 4: Реализовать `its/flows.py`**

```python
# toolgate/its/flows.py
"""ИТС login/search/read flows. Delegates browser work to BrowserDriver."""
import asyncio
import time
import urllib.parse

from .extract import extract_content, parse_search_results


class ItsBusy(Exception):
    """Сессия занята (вероятно, человеком) — консервативный перехват, S1."""


class ItsLoginFailed(Exception):
    """Логин не удался (креды/капча/2FA)."""


class ItsFlows:
    def __init__(self, driver, cfg: dict, now_fn=time.monotonic):
        self._d = driver
        self._cfg = cfg
        self._now = now_fn
        self._last_login_at = -1e9

    async def _is_logged_out(self) -> bool:
        url = await self._d.current_url()
        return self._cfg["logged_out"]["url_contains"] in url

    async def ensure_logged_in(self, creds: dict) -> None:
        await self._d.navigate(self._cfg["auth_probe_url"])
        if not await self._is_logged_out():
            return
        # Консервативный перехват: не логинимся чаще cooldown
        if self._now() - self._last_login_at < self._cfg["relogin_cooldown_s"]:
            raise ItsBusy("ИТС-сессия занята (вероятно, используется человеком); попробуйте позже")
        lc = self._cfg["login"]
        await self._d.fill(lc["login_selector"], creds["login"])
        await self._d.fill(lc["password_selector"], creds["password"])
        await self._d.click(lc["submit_selector"])
        self._last_login_at = self._now()
        await asyncio.sleep(1.0)  # человеческий темп + время редиректа
        url = await self._d.current_url()
        if lc["success_url_contains"] not in url:
            raise ItsLoginFailed(f"после логина остались на {url}")

    async def search(self, query: str, db: str | None = None) -> list[dict]:
        sc = self._cfg["search"]
        q = urllib.parse.quote(query)
        url = sc["url_template"].format(base=self._cfg["base_url"], q=q)
        if db and sc.get("db_scoped"):
            url += f"&db={urllib.parse.quote(db)}"
        await self._d.navigate(url)
        if sc.get("results_wait"):
            try:
                await self._d.wait(sc["results_wait"], timeout=15)
            except Exception:
                pass
        html = (await self._d.content())["html"]
        return parse_search_results(html, sc)

    async def read(self, ref: str) -> dict:
        rc = self._cfg["read"]
        if rc.get("print_url_template"):   # путь (a)
            url = rc["print_url_template"].format(base=self._cfg["base_url"], ref=ref)
        else:                              # путь (b)
            url = rc["full_url_template"].format(base=self._cfg["base_url"], ref=ref)
        await self._d.navigate(url)
        if rc.get("wait_selector"):
            try:
                await self._d.wait(rc["wait_selector"], timeout=15)
            except Exception:
                pass
        page = await self._d.content()
        out = extract_content(page["html"], rc["content_selector"], rc["strip_selectors"])
        out["url"] = page.get("url", url)
        return out
```

- [ ] **Step 5: Прогнать — зелёные** → PASS

- [ ] **Step 6: Commit**

```bash
git add toolgate/its/site.py toolgate/its/flows.py toolgate/tests/its/test_flows.py
git commit -m "feat(toolgate/its): login/search/read flows with conservative relogin (S1)"
```

### Task 2.6: `its/router.py` — FastAPI `/its/search`, `/its/read` + сериализация

**Files:**
- Create: `toolgate/its/router.py`
- Test: `toolgate/tests/its/test_router.py`

**Interfaces:**
- Consumes: `BrowserDriver`, `ItsFlows`, `TTLCache`, `get_credentials`, `SITE_ITS`.
- Produces: `router: APIRouter` с
  - `POST /its/search {query, db?}` → `{"results": [...]}`
  - `POST /its/read {ref}` → `{"title","markdown","url","images_omitted"}`
  - Глобальный `asyncio.Lock` + `asyncio.wait_for` (hard timeout, S5). Ошибки → структурный JSON: `its_busy` (409), `its_login_failed` (502), `its_timeout` (504).
- Модульная фабрика `def build_flows(http) -> ItsFlows` (для инъекции/тестов).

- [ ] **Step 1: Падающий тест (TestClient + подмена flows)**

```python
# toolgate/tests/its/test_router.py
import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
import its.router as itsr
from its.flows import ItsBusy

class FakeFlows:
    def __init__(self, results=None, read=None, busy=False):
        self._results = results or []; self._read = read or {}; self._busy = busy
    async def ensure_logged_in(self, creds):
        if self._busy: raise ItsBusy("busy")
    async def search(self, query, db=None): return self._results
    async def read(self, ref): return self._read

def make_client(flows):
    app = FastAPI()
    app.include_router(itsr.router)
    async def fake_build(http): return flows
    itsr.build_flows = fake_build
    async def fake_creds(http): return {"login": "u", "password": "p"}
    itsr.get_credentials = fake_creds
    app.state.http_client = object()
    return TestClient(app)

def test_search_returns_results():
    c = make_client(FakeFlows(results=[{"title": "T", "ref": "r", "snippet": "s", "db": "v854doc"}]))
    r = c.post("/its/search", json={"query": "регламент"})
    assert r.status_code == 200
    assert r.json()["results"][0]["title"] == "T"

def test_read_returns_markdown():
    c = make_client(FakeFlows(read={"title": "T", "markdown": "# T", "url": "u", "images_omitted": 0}))
    r = c.post("/its/read", json={"ref": "db/v854doc#bookmark:adm:TI1"})
    assert r.status_code == 200
    assert r.json()["markdown"] == "# T"

def test_busy_returns_409():
    c = make_client(FakeFlows(busy=True))
    r = c.post("/its/read", json={"ref": "x"})
    assert r.status_code == 409
    assert r.json()["error"] == "its_busy"
```

- [ ] **Step 2: Прогнать — падает** → FAIL

- [ ] **Step 3: Реализовать `its/router.py`**

```python
# toolgate/its/router.py
"""Agent-facing ИТС endpoints. Serialized single session; hard timeout."""
import asyncio
import logging

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from .driver import BrowserDriver
from .flows import ItsFlows, ItsBusy, ItsLoginFailed
from .cache import TTLCache
from .creds import get_credentials
from .site import SITE_ITS

log = logging.getLogger("toolgate.its")
router = APIRouter(tags=["its"])

_lock = asyncio.Lock()
_cache = TTLCache()
_OP_TIMEOUT_S = 90.0


class SearchReq(BaseModel):
    query: str
    db: str | None = None


class ReadReq(BaseModel):
    ref: str


async def build_flows(http) -> ItsFlows:
    return ItsFlows(BrowserDriver(http), SITE_ITS)


async def _run(http, coro_factory):
    creds = await get_credentials(http)
    if not creds:
        return JSONResponse(status_code=502,
                            content={"error": "its_no_credentials",
                                     "message": "ITS_CREDENTIALS не заданы в vault"})
    async with _lock:
        try:
            flows = await build_flows(http)
            await asyncio.wait_for(flows.ensure_logged_in(creds), timeout=_OP_TIMEOUT_S)
            return await asyncio.wait_for(coro_factory(flows), timeout=_OP_TIMEOUT_S)
        except ItsBusy as e:
            return JSONResponse(status_code=409, content={"error": "its_busy", "message": str(e)})
        except ItsLoginFailed as e:
            return JSONResponse(status_code=502, content={"error": "its_login_failed", "message": str(e)})
        except asyncio.TimeoutError:
            return JSONResponse(status_code=504, content={"error": "its_timeout",
                                "message": f"операция превысила {_OP_TIMEOUT_S:.0f}s"})
        except Exception as e:
            log.warning("its error: %s", e)
            return JSONResponse(status_code=502, content={"error": "its_error", "message": str(e)})


@router.post("/its/search")
async def its_search(body: SearchReq, request: Request):
    http = request.app.state.http_client
    ck = f"s:{body.db or ''}:{body.query.strip().lower()}"
    cached = _cache.get(ck)
    if cached is not None:
        return {"results": cached, "cached": True}

    async def do(flows):
        rows = await flows.search(body.query, body.db)
        _cache.set(ck, rows, SITE_ITS["search_cache_ttl_s"])
        return {"results": rows}
    return await _run(http, do)


@router.post("/its/read")
async def its_read(body: ReadReq, request: Request):
    http = request.app.state.http_client
    ck = f"r:{body.ref.strip()}"
    cached = _cache.get(ck)
    if cached is not None:
        return {**cached, "cached": True}

    async def do(flows):
        out = await flows.read(body.ref)
        _cache.set(ck, out, SITE_ITS["read_cache_ttl_s"])
        return out
    return await _run(http, do)
```

- [ ] **Step 4: Прогнать — зелёные** → PASS

- [ ] **Step 5: Commit**

```bash
git add toolgate/its/router.py toolgate/tests/its/test_router.py
git commit -m "feat(toolgate/its): serialized /its/search + /its/read endpoints with hard timeout"
```

### Task 2.7: Зарегистрировать роутер в `app.py`

**Files:**
- Modify: `toolgate/app.py:231-246` (импорт + include_router)

- [ ] **Step 1: Добавить импорт и include**

В `toolgate/app.py`, к строке импорта роутеров (231) добавить `its`-пакет и include:

```python
from its import router as its_router
```

и после `app.include_router(bcs.router)` (строка 246):

```python
app.include_router(its_router.router)
```

Экспортировать `router` из пакета — в `toolgate/its/__init__.py`:

```python
from . import router
```

- [ ] **Step 2: Smoke — приложение импортируется и роут есть**

Run:
```bash
cd toolgate && python -c "from app import app; paths=[r.path for r in app.routes]; assert '/its/search' in paths and '/its/read' in paths; print('OK', [p for p in paths if p.startswith('/its')])"
```
Expected: `OK ['/its/search', '/its/read']`

- [ ] **Step 3: Прогнать весь toolgate-пакет тестов (регресс)**

Run: `cd toolgate && python -m pytest tests/its -v && python -m pytest -q`
Expected: PASS (ITS-тесты + без регрессий существующих)

- [ ] **Step 4: Commit**

```bash
git add toolgate/app.py toolgate/its/__init__.py
git commit -m "feat(toolgate): mount ITS router (/its/search, /its/read)"
```

---

## Phase 3 — core: internal-эндпоинт кредов

### Task 3.1: `GET /api/internal/its-credentials` (чтение из vault)

**Files:**
- Create: `crates/opex-core/src/gateway/handlers/internal_creds.rs`
- Modify: `crates/opex-core/src/gateway/handlers/mod.rs` (объявить модуль + merge routes)
- Test: unit в `internal_creds.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `AppState` (доступ к `SecretsManager` — resolve `ITS_CREDENTIALS`, scope global).
- Produces: `pub(crate) fn routes() -> Router<AppState>` c `GET /api/internal/its-credentials` → JSON `{login, password}` (200) или 404, если секрет не задан. Защищён общей auth-middleware (у toolgate есть токен; loopback exempt). Секрет — JSON `{"login","password"}` в vault.

- [ ] **Step 1: Найти паттерн подключения sub-router**

Run: `grep -n "pub(crate) fn routes" crates/opex-core/src/gateway/handlers/misc.rs | head -1`
Изучить, как `mod.rs` мёржит роутеры (`.merge(misc::routes())`), и как handler достаёт `SecretsManager` из `AppState` (см. `state.rs` / существующие handlers).

- [ ] **Step 2: Написать падающий тест — парсинг секрета**

```rust
// в crates/opex-core/src/gateway/handlers/internal_creds.rs, #[cfg(test)]
#[test]
fn parses_credentials_json() {
    let v: ItsCreds = serde_json::from_str(r#"{"login":"u","password":"p"}"#).unwrap();
    assert_eq!(v.login, "u");
    assert_eq!(v.password, "p");
}
```

- [ ] **Step 3: Прогнать — падает**

Run: `cargo test -p opex-core internal_creds` → FAIL (тип/модуль не существует)

- [ ] **Step 4: Реализовать handler**

```rust
// crates/opex-core/src/gateway/handlers/internal_creds.rs
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use crate::gateway::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ItsCreds {
    pub login: String,
    pub password: String,
}

async fn get_its_credentials(State(state): State<AppState>) -> impl IntoResponse {
    // Резолвим секрет ITS_CREDENTIALS (JSON) из vault, scope global ("").
    match state.secrets.resolve("ITS_CREDENTIALS", "").await {
        Some(raw) => match serde_json::from_str::<ItsCreds>(&raw) {
            Ok(c) => Json(c).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR,
                       format!("ITS_CREDENTIALS malformed: {e}")).into_response(),
        },
        None => (StatusCode::NOT_FOUND, "ITS_CREDENTIALS not set").into_response(),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/internal/its-credentials", get(get_its_credentials))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_credentials_json() {
        let v: ItsCreds = serde_json::from_str(r#"{"login":"u","password":"p"}"#).unwrap();
        assert_eq!(v.login, "u");
        assert_eq!(v.password, "p");
    }
}
```

> Точную сигнатуру `state.secrets.resolve(...)` сверить со Step 1 (метод `SecretsManager`). Если API иное (напр. `get`/`resolve_scoped`) — адаптировать вызов; тест на парсинг от этого не зависит.

- [ ] **Step 5: Подключить модуль в `mod.rs`**

В `crates/opex-core/src/gateway/handlers/mod.rs` добавить `mod internal_creds;` и в композицию роутов `.merge(internal_creds::routes())` (рядом с прочими `.merge(...)`).

- [ ] **Step 6: Прогнать — зелёные + сборка**

Run: `cargo test -p opex-core internal_creds && cargo check -p opex-core`
Expected: PASS + чистая сборка

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/internal_creds.rs crates/opex-core/src/gateway/handlers/mod.rs
git commit -m "feat(core): GET /api/internal/its-credentials — resolve ITS_CREDENTIALS from vault"
```

---

## Phase 4 — агентский YAML-tool

### Task 4.1: `workspace/tools/its.yaml`

**Files:**
- Create: `workspace/tools/its.yaml`

**Interfaces:**
- Consumes: toolgate `/its/search`, `/its/read` (Phase 2).
- Produces: агентский инструмент `its` (action=search|read). Endpoint `http://localhost:9011/its/...` — `is_internal_endpoint` → стандартный http_client.

- [ ] **Step 1: Создать YAML (шаблон по образцу `workspace/tools/web.yaml`)**

Так как один эндпоинт на действие, делаем два маршрута через `action` + `body_template`. Проверенный в репо паттерн — отдельный tool на endpoint; используем два действия через разные пути. Реализуем как **один tool с параметром `action`**, маршрутизирующий на под-путь:

```yaml
name: its
description: "Поиск и чтение документации 1С на its.1c.ru (платный ИТС). action=search: найти по запросу, возвращает список {title,snippet,ref}. action=read: прочитать страницу/закладку по ref (из результатов поиска или полный URL), возвращает markdown. Единая залогиненная сессия, вежливый темп."
endpoint: "http://localhost:9011/its/{{action}}"
method: POST

parameters:
  action:
    type: string
    required: true
    location: path
    description: "search — поиск; read — чтение"
    enum: [search, read]
  query:
    type: string
    required: false
    location: body
    description: "Поисковый запрос (для action=search)"
  db:
    type: string
    required: false
    location: body
    description: "Ограничить поиск базой (напр. v854doc), опционально"
  ref:
    type: string
    required: false
    location: body
    description: "Ссылка/закладка для чтения (для action=read), напр. db/v854doc#bookmark:adm:TI000001410"

body_template: |
  {"query": "{{query}}", "db": "{{db}}", "ref": "{{ref}}"}

status: draft
tags: [1c, its, docs, search]
```

> Если раннер YAML-tools не поддерживает path-параметр `{{action}}` в `endpoint` — разбить на два tool-файла `its_search.yaml` (endpoint `/its/search`) и `its_read.yaml` (endpoint `/its/read`), контракт для агента эквивалентен. Проверить в Step 2.

- [ ] **Step 2: Проверить загрузку tool (unit-раннер YAML-tools)**

Run:
```bash
grep -rn "location: path" workspace/tools/ | head   # подтвердить поддержку path-параметра в существующих tools
```
Если path-параметров ни у кого нет — переключиться на два файла (см. заметку выше) и убрать `location: path`.

- [ ] **Step 3: Commit**

```bash
git add workspace/tools/its.yaml
git commit -m "feat(tools): add agent-facing 'its' YAML tool (search/read via toolgate)"
```

---

## Phase 5 — деплой, assisted-login, live E2E

### Task 5.1: Operator assisted-login / посев кук (условно — если Phase 0 показал капчу/2FA)

Активируется, только если findings §0 зафиксировали капчу/2FA. Иначе — пропустить (автологин достаточен).

**Files:**
- Create: `docker/browser-renderer/app.py` — эндпоинт `POST /automation {action:"screenshot", profile:"its"}` уже работает (Task 1.3); для ручного входа оператор гоняет headful локально с тем же `user_data_dir`.
- Create (в git): `docs/superpowers/specs/2026-07-03-its-assisted-login.md` — инструкция оператору.

- [ ] **Step 1: Написать инструкцию посева профиля**

`docs/superpowers/specs/2026-07-03-its-assisted-login.md`:

```markdown
# ИТС assisted login (разовый посев профиля)
Если автологин упирается в капчу/2FA:
1. На сервере остановить browser-renderer.
2. Запустить локально Playwright headful с тем же user_data_dir, что и том
   browser_profiles (папка its): выполнить вход руками (капча/2FA).
3. Скопировать папку профиля в том browser_profiles (/profiles/its).
4. Поднять browser-renderer — куки на месте, автомат живёт на них.
Обновлять при протухании сессии тем же способом.
```

- [ ] **Step 2: Commit**

```bash
git add -f docs/superpowers/specs/2026-07-03-its-assisted-login.md
git commit -m "docs(its): operator assisted-login / cookie-seed procedure"
```

### Task 5.2: Деплой + положить креды в vault + live E2E (приёмка)

**Files:** нет новых — процедура.

- [ ] **Step 1: Положить креды в vault (НЕ в git)**

Через secrets API core (пример; подставить реальные логин/пароль оператора):

```bash
curl -sS -X POST "$CORE/api/secrets" -H "Authorization: Bearer $OPEX_AUTH_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"ITS_CREDENTIALS","scope":"","value":"{\"login\":\"<LOGIN>\",\"password\":\"<PASSWORD>\"}"}'
```
Проверить: `curl -sS "$CORE/api/internal/its-credentials" -H "Authorization: Bearer $OPEX_AUTH_TOKEN"` → `{"login":...,"password":...}`.

> Точный путь/поля secrets API сверить с `gateway/handlers/secrets*`/UI Vault.

- [ ] **Step 2: Пересобрать browser-renderer + деплой core/toolgate**

По memory (deploy gaps): docker-образы билдятся из `~/opex/docker/` на сервере; server-deploy синкает Rust-бинарники + toolgate `.py`, НЕ синкает docker автоматически.

```bash
# на сервере:
cd ~/opex/docker && docker compose build browser-renderer && docker compose up -d browser-renderer
# core (эндпоинт кредов): make remote-deploy   (pull → build → swap → restart)
# toolgate (новый пакет its/): синкнуть toolgate/ на сервер + POST /api/services/toolgate/restart
```

- [ ] **Step 2a: Скопировать пакет `toolgate/its/` на сервер**

server-deploy синкает изменённые `.py`, но новый подкаталог `its/` подтвердить вручную:

```bash
rsync -a toolgate/its/ aronmav@188.246.224.118:~/opex/toolgate/its/
ssh aronmav@188.246.224.118 "curl -sS -X POST http://127.0.0.1:18789/api/services/toolgate/restart -H 'Authorization: Bearer '$OPEX_AUTH_TOKEN"
```

- [ ] **Step 3: Live E2E (приёмка)**

```bash
CORE=https://<core-host>; H="Authorization: Bearer $OPEX_AUTH_TOKEN"
# 1) read референс-закладки
curl -sS -X POST "$CORE/../its/read" -H "$H" -H 'Content-Type: application/json' \
  -d '{"ref":"db/v854doc#bookmark:adm:TI000001410"}' | jq '{title, images_omitted, md_len: (.markdown|length)}'
# ожидаем: title непустой, markdown содержательный (не логин-страница)
# 2) search
curl -sS -X POST "$CORE/../its/read" ...  # аналогично /its/search с {"query":"регламентное задание"}
```

Критерий приёмки: `its_read` референс-закладки вернул **чистый markdown документа** (а не форму логина / пустоту), `its_search` вернул непустой список с валидными `ref`.

- [ ] **Step 4: Прогнать агента E2E в чате**

В UI/канале дать агенту задачу «найди в документации 1С про регламентные задания и приведи выдержку» → агент вызывает `its` (search→read) → отдаёт содержательный ответ. Зафиксировать результат.

- [ ] **Step 5: Финальный commit статуса**

```bash
git add docs/superpowers/specs/2026-07-03-its-1c-browser-tool-design.md
git commit -m "docs(its): mark ITS browser tool deployed + E2E verified" --allow-empty
```

---

## Self-Review (выполнено при написании плана)

**1. Покрытие спеки:**
- §3.1 generic browser-renderer (профили + stealth) → Tasks 1.1–1.4 ✅
- §3.2 оркестратор toolgate → Tasks 2.1–2.7 ✅
- §3.3 агентский tool → Task 4.1 ✅
- §4.1 ленивый логин + консервативный перехват (S1) → Task 2.5 (`ensure_logged_in`, cooldown, ItsBusy) ✅
- §4.2 assisted-login (S2) → Task 5.1 ✅
- §4.3 search / §4.4 read (a/b + картинки S7) → Tasks 2.1, 2.5 ✅
- §5 анти-бот (stealth/темп) → Tasks 1.2, 2.5 ✅
- §6 креды (S4) → Tasks 2.4 + 3.1 ✅
- §7 кэш (+канонизация S10) → Task 2.2, ключи в 2.6 ✅
- §8 сериализация + hard timeout (S5) → Task 2.6 (`_lock`, `wait_for`) ✅
- §9 ошибки (its_busy/its_timeout/login_failed) → Task 2.6 ✅
- §10 тесты (fake-page, фикстуры, E2E, live-smoke S11) → Tasks 1.x/2.x, 5.2 ✅
- §11 деплой (том, docker, toolgate) → Tasks 1.4, 5.2 ✅
- §12 spike → Phase 0 ✅

**2. Плейсхолдеры:** site-специфичные селекторы в `SITE_ITS` (Task 2.5) — это **данные из Phase 0**, помеченные `← из findings`, а не «TODO в логике»: код и тесты конкретны (тесты на синтетическом HTML + мок-драйвере). Путь read (a/b) реализован обоими ветками, выбор — по конфигу. Допустимо для spike-first плана.

**3. Согласованность типов:** `BrowserDriver` (методы navigate/fill/click/wait/content/current_url) — консистентны между 2.3 → 2.5 (FakeDriver повторяет сигнатуры) → 2.6. `ItsFlows.{ensure_logged_in,search,read}` + `ItsBusy/ItsLoginFailed` — одни и те же в 2.5 → 2.6. `extract_content`/`parse_search_results` сигнатуры совпадают 2.1 → 2.5. `get_credentials(http)` — 2.4 → 2.6. `create_session(profile=...)` — 1.1 → 1.3 → 2.3. ✅

**Открытые сверки для исполнителя (не блокеры):** точная сигнатура `SecretsManager::resolve` (Task 3.1 Step 1), поддержка path-параметра в YAML-tools (Task 4.1 Step 2), точный secrets API (Task 5.2 Step 1) — каждая помечена в своей задаче.
