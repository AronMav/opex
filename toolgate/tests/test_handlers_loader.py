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

# ── Hot-reload fixtures ──────────────────────────────────────────────────────

WS_V1 = textwrap.dedent('''\
    # <handler>
    #   <id>myhandler</id>
    #   <label lang="en">My Handler v1</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("v1")
''')

WS_V2 = textwrap.dedent('''\
    # <handler>
    #   <id>myhandler</id>
    #   <label lang="en">My Handler v2</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("v2")
''')

WS_RENAMED = textwrap.dedent('''\
    # <handler>
    #   <id>myhandler-renamed</id>
    #   <label lang="en">Renamed Handler</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("renamed")
''')

WS_COLLIDE_BUILTIN = textwrap.dedent('''\
    # <handler>
    #   <id>echo</id>
    #   <label lang="en">Workspace Echo</label>
    #   <match><mime>text/*</mime></match>
    #   <execution>sync</execution>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("ws-echo")
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


def test_workspace_overrides_builtin_id(tmp_path):
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
    # workspace override is effective: get() returns the workspace version
    lh = reg.get("echo")
    assert lh is not None
    assert lh.tier == "workspace", "get() tier reflects the effective handler (workspace)"
    assert lh.descriptor.labels.get("en") == "Shadow", "workspace body is active"
    # manifest derives tier from builtin-id membership and reports source=override
    m = {x["id"]: x for x in reg.manifests()}
    assert m["echo"]["tier"] == "builtin", "manifest tier stays builtin for gating"
    assert m["echo"]["source"] == "override"


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
    assert item["command"] is None  # no <command> override in GOOD fixture
    e1 = reg.etag()
    assert isinstance(e1, str) and e1
    # stable for identical content
    assert reg.etag() == e1


WITH_COMMAND = textwrap.dedent('''\
    # <handler>
    #   <id>summarize_video</id>
    #   <label lang="en">Summarize</label>
    #   <match><mime>video/*</mime></match>
    #   <execution>async</execution>
    #   <command name="sumvid" aliases="sv,summary"/>
    # </handler>

    async def run(ctx, file, params):
        return ctx.result.text("ok")
''')


def test_manifest_includes_command_override(tmp_path):
    """A handler with a <command> override surfaces it in the /handlers manifest."""
    builtin = tmp_path / "builtin"
    builtin.mkdir()
    _write(builtin, "summarize_video.py", WITH_COMMAND)
    reg = HandlerRegistry()
    reg.load_all(str(builtin), None)
    m = {x["id"]: x for x in reg.manifests()}
    assert m["summarize_video"]["command"] == {"name": "sumvid", "aliases": ["sv", "summary"]}


# ── Hot-reload tests (new) ───────────────────────────────────────────────────

def test_reload_file_updates_existing_workspace_handler(tmp_path):
    """reload_file on an edited workspace file updates get(id) to new version."""
    ws = tmp_path / "workspace"
    fh = ws / "file_handlers"
    fh.mkdir(parents=True)
    p = _write(fh, "myhandler.py", WS_V1)

    reg = HandlerRegistry()
    reg.load_all(str(tmp_path / "builtin_empty"), str(ws))  # no builtin dir
    assert reg.get("myhandler") is not None
    assert reg.get("myhandler").descriptor.labels["en"] == "My Handler v1"

    # Simulate file edit: overwrite with v2
    p.write_text(WS_V2, encoding="utf-8")
    reg.reload_file(str(p))

    lh = reg.get("myhandler")
    assert lh is not None
    assert lh.descriptor.labels["en"] == "My Handler v2"
    # Only one entry for this id
    assert len([m for m in reg.manifests() if m["id"] == "myhandler"]) == 1


def test_reload_file_id_renamed_evicts_old_registers_new(tmp_path):
    """reload_file when the file's id changes evicts old id and registers new."""
    ws = tmp_path / "workspace"
    fh = ws / "file_handlers"
    fh.mkdir(parents=True)
    p = _write(fh, "myhandler.py", WS_V1)

    reg = HandlerRegistry()
    reg.load_all(str(tmp_path / "builtin_empty"), str(ws))
    assert reg.get("myhandler") is not None

    # Overwrite same file but id changed to myhandler-renamed
    p.write_text(WS_RENAMED, encoding="utf-8")
    reg.reload_file(str(p))

    assert reg.get("myhandler") is None, "old id must be evicted"
    lh = reg.get("myhandler-renamed")
    assert lh is not None
    assert lh.descriptor.labels["en"] == "Renamed Handler"


def test_reload_file_workspace_overrides_builtin(tmp_path):
    """reload_file of a workspace file whose id matches a builtin installs an override."""
    builtin_dir = tmp_path / "builtin"
    builtin_dir.mkdir()
    _write(builtin_dir, "echo.py", GOOD)

    ws = tmp_path / "workspace"
    fh = ws / "file_handlers"
    fh.mkdir(parents=True)
    p = _write(fh, "ws_echo.py", WS_COLLIDE_BUILTIN)

    reg = HandlerRegistry()
    reg.load_all(str(builtin_dir), str(ws))
    # override installed at load_all time: workspace body is effective
    lh = reg.get("echo")
    assert lh.tier == "workspace"
    assert lh.descriptor.labels.get("en") == "Workspace Echo"

    # Hot-reload installs the override again (idempotent)
    reg.reload_file(str(p))
    lh = reg.get("echo")
    assert lh.tier == "workspace"
    assert lh.descriptor.labels.get("en") == "Workspace Echo"
    m = {x["id"]: x for x in reg.manifests()}
    assert m["echo"]["source"] == "override"

    # remove_file resets to pristine builtin
    import os
    os.remove(str(p))
    reg.remove_file(str(p))
    lh = reg.get("echo")
    assert lh.tier == "builtin"
    m = {x["id"]: x for x in reg.manifests()}
    assert m["echo"]["source"] == "builtin"


def test_remove_file_evicts_workspace_handler(tmp_path):
    """remove_file evicts the handler; get(id) returns None and not in manifests."""
    ws = tmp_path / "workspace"
    fh = ws / "file_handlers"
    fh.mkdir(parents=True)
    p = _write(fh, "myhandler.py", WS_V1)

    reg = HandlerRegistry()
    reg.load_all(str(tmp_path / "builtin_empty"), str(ws))
    assert reg.get("myhandler") is not None

    reg.remove_file(str(p))

    assert reg.get("myhandler") is None
    ids_in_manifests = [m["id"] for m in reg.manifests()]
    assert "myhandler" not in ids_in_manifests


def test_remove_file_unknown_path_noop(tmp_path):
    """remove_file with an unknown path is a safe no-op."""
    reg = HandlerRegistry()
    reg.load_all(str(tmp_path / "builtin_empty"), None)
    # Should not raise
    reg.remove_file(str(tmp_path / "nonexistent.py"))
