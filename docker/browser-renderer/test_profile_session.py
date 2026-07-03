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
