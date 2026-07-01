import os
from handlers.loader import HandlerRegistry

BUILTIN = '''# <handler>
#   <id>describe</id>
#   <label lang="en">Describe</label>
#   <match><mime>image/*</mime></match>
#   <execution>sync</execution>
# </handler>
async def run(ctx, file, params):
    return ctx.result.ok("builtin")
'''

OVERRIDE = BUILTIN.replace('ctx.result.ok("builtin")', 'ctx.result.ok("override")')

NEWWS = '''# <handler>
#   <id>my_ocr</id>
#   <label lang="en">OCR</label>
#   <match><mime>image/*</mime></match>
#   <execution>sync</execution>
# </handler>
async def run(ctx, file, params):
    return ctx.result.ok("ws")
'''


def _write(p, s):
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w", encoding="utf-8") as f:
        f.write(s)


def test_workspace_shadows_builtin_and_reset_resurfaces(tmp_path):
    bdir = tmp_path / "builtin"
    wdir = tmp_path / "ws"
    _write(str(bdir / "describe.py"), BUILTIN)
    reg = HandlerRegistry()
    reg.load_all(str(bdir), str(wdir))

    # pristine builtin
    m = {x["id"]: x for x in reg.manifests()}
    assert m["describe"]["source"] == "builtin"
    assert m["describe"]["tier"] == "builtin"

    # add an override → shadows the builtin
    ov = str(wdir / "file_handlers" / "describe.py")
    _write(ov, OVERRIDE)
    reg.reload_file(ov)
    m = {x["id"]: x for x in reg.manifests()}
    assert m["describe"]["source"] == "override"
    assert m["describe"]["tier"] == "builtin", "override keeps the builtin id/tier for gating"

    # remove the override → the pristine builtin resurfaces (reset to default)
    os.remove(ov)
    reg.remove_file(ov)
    m = {x["id"]: x for x in reg.manifests()}
    assert m["describe"]["source"] == "builtin"


def test_new_workspace_id_is_workspace_tier(tmp_path):
    bdir = tmp_path / "builtin"
    wdir = tmp_path / "ws"
    os.makedirs(str(bdir), exist_ok=True)
    _write(str(wdir / "file_handlers" / "my_ocr.py"), NEWWS)
    reg = HandlerRegistry()
    reg.load_all(str(bdir), str(wdir))
    m = {x["id"]: x for x in reg.manifests()}
    assert m["my_ocr"]["source"] == "workspace"
    assert m["my_ocr"]["tier"] == "workspace"
