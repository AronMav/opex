from config import ProvidersConfig


def test_workspace_dir_defaults_none():
    cfg = ProvidersConfig()
    assert cfg.workspace_dir is None


def test_workspace_dir_parsed_from_payload():
    cfg = ProvidersConfig(**{
        "version": 1,
        "active": {},
        "providers": {},
        "workspace_dir": "/home/aronmav/opex/workspace",
    })
    assert cfg.workspace_dir == "/home/aronmav/opex/workspace"


def test_workspace_dir_none_when_absent():
    cfg = ProvidersConfig(**{"version": 1, "active": {}, "providers": {}})
    assert cfg.workspace_dir is None
