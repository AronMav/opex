import pytest
from fastapi.testclient import TestClient

@pytest.fixture
def client(monkeypatch):
    import app
    class FakePage:
        def __init__(self): self.url = "about:blank"; self.closed = False
        def on(self, *a, **k): pass
        def is_closed(self): return self.closed
        async def close(self): self.closed = True
        async def add_init_script(self, js): self.init_js = js
    class FakeCtx:
        def __init__(self, udd): self.user_data_dir = udd; self.pages_made = []
        @property
        def pages(self): return [p for p in self.pages_made if not p.closed]
        async def new_page(self):
            p = FakePage(); self.pages_made.append(p); return p
        async def add_init_script(self, js): self.init_js = js
        async def close(self): pass
    async def fake_factory(udd): return FakeCtx(udd)
    # Подменяем фабрику профиля и общий browser; чистим module-global состояние
    app.profile_manager = app.ProfileManager(factory=fake_factory, root="/tmp/pf")
    class FakeBrowser:
        async def new_page(self, **k): return FakePage()
    app.browser = FakeBrowser()
    app.sessions.clear()
    app.session_last_used.clear()
    app.session_dialog.clear()
    app.persistent_sessions.clear()
    return TestClient(app.app)

def test_create_profile_session_uses_persistent_context(client):
    r = client.post("/automation", json={"action": "create_session", "profile": "its"})
    assert r.status_code == 200
    assert r.json()["status"] == "created"

def test_create_session_without_profile_still_works(client):
    r = client.post("/automation", json={"action": "create_session"})
    assert r.status_code == 200
    assert "session_id" in r.json()

def test_recreate_profile_session_closes_previous_pages(client):
    # Регрессия: persistent-страницы не имеют idle-TTL; без закрытия старых
    # вкладок каждый новый владелец профиля (рестарт toolgate) копит их до
    # mem_limit контейнера. Новая сессия того же профиля забирает его целиком.
    import app
    sid1 = client.post("/automation",
                       json={"action": "create_session", "profile": "its"}).json()["session_id"]
    sid2 = client.post("/automation",
                       json={"action": "create_session", "profile": "its"}).json()["session_id"]
    assert sid1 != sid2
    assert sid1 not in app.sessions            # старый sid выселен
    assert sid1 not in app.persistent_sessions
    assert sid2 in app.sessions
    assert len(app.sessions) == 1              # ровно одна живая вкладка профиля
    # старая страница реально закрыта
    r = client.post("/automation", json={"action": "content", "session_id": sid1})
    assert r.status_code == 404
