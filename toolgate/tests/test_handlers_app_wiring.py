"""Tests for HandlerRegistry wiring in app.py lifespan + /handlers router mount."""

import importlib
import os

from fastapi.testclient import TestClient


async def _async_noop(self):
    # The real warm-up path: app.py lifespan calls registry.aload(), which
    # delegates to ProviderRegistry._refresh() — that does an INLINE
    # httpx GET to {CORE_API_URL}/api/media-config. Stub the bound method so
    # lifespan startup never touches DNS/network (the autouse _clear_legacy_env
    # fixture points CORE_API_URL at http://core-test:18789). The registry keeps
    # its constructor default self.config = ProvidersConfig() (workspace_dir=None),
    # so load_all is called with the builtin dir + ws_dir=None.
    return None


def test_app_mounts_handlers_and_state(monkeypatch):
    # Keep registry warm-up from touching the network (stub the real _refresh).
    monkeypatch.setattr("registry.ProviderRegistry._refresh", _async_noop)
    import app as app_module
    importlib.reload(app_module)
    with TestClient(app_module.app) as client:
        # app.state.handlers is populated in lifespan with the builtin handlers
        assert hasattr(app_module.app.state, "handlers")
        ids = {m["id"] for m in app_module.app.state.handlers.manifests()}
        assert {"save", "transcribe", "describe", "extract_document"} <= ids
        # the router is mounted
        r = client.get("/handlers")
        assert r.status_code == 200
        got = {h["id"] for h in r.json()["handlers"]}
        assert {"save", "transcribe", "describe", "extract_document"} <= got


def test_builtin_dir_resolution(monkeypatch):
    monkeypatch.setattr("registry.ProviderRegistry._refresh", _async_noop)
    import app as app_module
    importlib.reload(app_module)
    # the helper must resolve to an existing directory containing the builtins
    d = app_module._builtin_handlers_dir()
    assert os.path.isdir(d)
    assert os.path.isfile(os.path.join(d, "transcribe.py"))
