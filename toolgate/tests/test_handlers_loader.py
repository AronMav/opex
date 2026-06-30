"""Unit tests for handlers.loader — HandlerRegistry."""

import textwrap
from pathlib import Path

from handlers.loader import HandlerRegistry, LoadedHandler

GOOD = textwrap.dedent('''\
    # <handler>
    #   <id>echo</id>
    #   <label lang="ru">Эхо</label>
    #   <label lang="en">Echo</label>
    #   <icon>file</icon>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    #   <output>text</output>
    #   <order>5</order>
    #   <enabled>true</enabled>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text(file.filename)
''')

SYNTAX_ERR = textwrap.dedent('''\
    # <handler>
    #   <id>broken</id>
    #   <label lang="en">Broken</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params)   # missing colon -> SyntaxError
        return ctx.result.text("x")
''')

DUP_BUILTIN = textwrap.dedent('''\
    # <handler>
    #   <id>echo</id>
    #   <label lang="en">Shadow</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("shadow")
''')


def _write(d: Path, name: str, body: str) -> Path:
    p = d / name
    p.write_text(body, encoding="utf-8")
    return p


def test_load_all_registers_builtin(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "echo.py", GOOD)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), None)
    lh = reg.get("echo")
    assert isinstance(lh, LoadedHandler)
    assert lh.tier == "builtin"
    assert lh.descriptor.id == "echo"
    assert callable(lh.run)


def test_syntax_error_file_skipped_not_crash(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "ok.py", GOOD)
    _write(builtin, "bad.py", SYNTAX_ERR)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), None)  # must not raise
    assert reg.get("echo") is not None
    assert reg.get("broken") is None


def test_workspace_cannot_shadow_builtin_id(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "echo.py", GOOD)
    ws = tmp_path / "workspace"
    ws.mkdir()
    fh = ws / "file_handlers"
    fh.mkdir()
    _write(fh, "shadow.py", DUP_BUILTIN)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), str(ws))
    # builtin wins; the workspace clash is rejected (still builtin tier)
    assert reg.get("echo").tier == "builtin"


def test_manifests_and_etag(tmp_path):
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "echo.py", GOOD)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), None)
    ms = reg.manifests()
    assert len(ms) == 1
    item = ms[0]
    assert item["id"] == "echo"
    assert item["labels"] == {"ru": "Эхо", "en": "Echo"}
    assert item["match"]["mime"] == ["text/*"]
    assert item["execution"] == "sync"
    assert item["tier"] == "builtin"
    assert item["provider"] is None  # router fills this from the active provider
    e1 = reg.etag()
    assert isinstance(e1, str) and e1
    # stable for identical content
    assert reg.etag() == e1
