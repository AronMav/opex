from handlers.validate import validate_source

GOOD = '''# <handler>
#   <id>my_ocr</id>
#   <label lang="en">OCR</label>
#   <match><mime>image/*</mime></match>
#   <execution>sync</execution>
# </handler>
async def run(ctx, file, params):
    return ctx.result.ok("hi")
'''

def test_valid_source_ok_with_descriptor():
    r = validate_source(GOOD, expected_id="my_ocr")
    assert r["ok"] is True
    assert r["errors"] == []
    assert r["descriptor"]["id"] == "my_ocr"
    assert r["descriptor"]["match"]["mime"] == ["image/*"]

def test_bad_descriptor_reports_error():
    r = validate_source("async def run(ctx, file, params):\n    return None\n")
    assert r["ok"] is False
    assert any(e["field"] == "descriptor" for e in r["errors"])

def test_bad_python_reports_error():
    src = GOOD.replace("async def run(ctx, file, params):", "async def run(:")
    r = validate_source(src)
    assert r["ok"] is False
    assert any(e["field"] == "python" for e in r["errors"])

def test_missing_run_reports_error():
    src = GOOD.rsplit("async def run", 1)[0] + "x = 1\n"
    r = validate_source(src)
    assert r["ok"] is False
    assert any("run" in e["message"] for e in r["errors"])

def test_id_mismatch_reports_error():
    r = validate_source(GOOD, expected_id="different")
    assert r["ok"] is False
    assert any(e["field"] == "id" for e in r["errors"])
