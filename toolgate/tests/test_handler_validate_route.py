from fastapi import FastAPI
from fastapi.testclient import TestClient

from handlers.router import router as handlers_router


def _client() -> TestClient:
    app = FastAPI()
    app.include_router(handlers_router)  # /handlers/validate needs no app.state
    return TestClient(app)


def test_validate_route_ok():
    src = ('# <handler>\n#   <id>my_ocr</id>\n#   <label lang="en">OCR</label>\n'
           '#   <match><mime>image/*</mime></match>\n#   <execution>sync</execution>\n'
           '# </handler>\nasync def run(ctx, file, params):\n    return None\n')
    r = _client().post("/handlers/validate", json={"source": src, "id": "my_ocr"})
    assert r.status_code == 200
    body = r.json()
    assert body["ok"] is True
    assert body["descriptor"]["id"] == "my_ocr"


def test_validate_route_reports_errors():
    r = _client().post("/handlers/validate", json={"source": "x = (", "id": "bad"})
    assert r.status_code == 200
    body = r.json()
    assert body["ok"] is False
    assert body["errors"]
