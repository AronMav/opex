"""Unit tests for /primitives/imap/* endpoints."""

from unittest.mock import MagicMock, patch

import pytest
from fastapi.testclient import TestClient


@pytest.fixture
def client():
    """TestClient over a FastAPI app that mounts only the IMAP primitive router."""
    from fastapi import FastAPI
    from primitives import imap

    app = FastAPI()
    app.include_router(imap.router)
    return TestClient(app)


def test_imap_fetch_rejects_missing_fields(client):
    """pydantic validation — missing user/password/server/etc. → 422."""
    resp = client.post("/primitives/imap/fetch", json={})
    assert resp.status_code == 422


@patch("primitives.imap.imaplib.IMAP4_SSL")
def test_imap_fetch_happy_path(mock_imap_cls, client):
    """Valid request → imaplib invoked with correct credentials, messages returned."""
    mock_imap = MagicMock()
    mock_imap_cls.return_value = mock_imap
    mock_imap.select.return_value = ("OK", [b"1"])

    def _uid_side_effect(cmd, *args):
        if cmd == "search":
            return ("OK", [b"42"])
        if cmd == "fetch":
            return ("OK", [(
                b"42 (RFC822 {123}",
                b"From: sender@test.com\r\nSubject: hi\r\nDate: Mon, 1 Apr 2026 10:00:00 +0000\r\n\r\nbody text"
            )])
        raise RuntimeError(f"unexpected uid cmd: {cmd}")
    mock_imap.uid.side_effect = _uid_side_effect
    mock_imap.close.return_value = ("OK", [])
    mock_imap.logout.return_value = ("BYE", [])

    resp = client.post("/primitives/imap/fetch", json={
        "server": "imap.test.com",
        "port": 993,
        "user": "me@test.com",
        "password": "secret",
        "folder": "INBOX",
        "limit": 10,
        "unread_only": False,
    })

    assert resp.status_code == 200, resp.text
    data = resp.json()
    assert "messages" in data
    assert len(data["messages"]) == 1
    assert data["messages"][0]["subject"] == "hi"
    assert data["messages"][0]["from"] == "sender@test.com"

    mock_imap_cls.assert_called_once_with("imap.test.com", 993, timeout=15)
    mock_imap.login.assert_called_once_with("me@test.com", "secret")


@patch("primitives.imap.imaplib.IMAP4_SSL")
def test_imap_fetch_auth_failure_returns_401(mock_imap_cls, client):
    """Login raises IMAP4.error → endpoint returns 401."""
    import imaplib
    mock_imap = MagicMock()
    mock_imap_cls.return_value = mock_imap
    mock_imap.login.side_effect = imaplib.IMAP4.error("auth failed")

    resp = client.post("/primitives/imap/fetch", json={
        "server": "imap.test.com", "port": 993,
        "user": "me@test.com", "password": "wrong",
    })
    assert resp.status_code == 401
    # FastAPI HTTPException puts message under "detail"
    assert "auth" in resp.json()["detail"].lower()


@patch("primitives.imap.imaplib.IMAP4_SSL")
def test_imap_search_happy_path(mock_imap_cls, client):
    mock_imap = MagicMock()
    mock_imap_cls.return_value = mock_imap
    mock_imap.select.return_value = ("OK", [b"1"])

    def _uid_side_effect(cmd, *args):
        if cmd == "search":
            return ("OK", [b"7 11"])
        if cmd == "fetch":
            return ("OK", [(
                b"11 (RFC822 {80}",
                b"From: a@b.com\r\nSubject: match\r\nDate: Mon, 1 Apr 2026 10:00:00 +0000\r\n\r\nbody"
            )])
        raise RuntimeError(f"unexpected uid cmd: {cmd}")
    mock_imap.uid.side_effect = _uid_side_effect
    mock_imap.close.return_value = ("OK", [])
    mock_imap.logout.return_value = ("BYE", [])

    resp = client.post("/primitives/imap/search", json={
        "server": "imap.test.com", "port": 993,
        "user": "me@test.com", "password": "secret",
        "query": "invoice", "limit": 5,
    })

    assert resp.status_code == 200, resp.text
    data = resp.json()
    assert data["count"] >= 1
    # Verify the CHARSET UTF-8 path was attempted with the query as UTF-8 bytes.
    mock_imap.uid.assert_any_call("search", "CHARSET", "UTF-8", "TEXT", b"invoice")


@patch("primitives.imap.imaplib.IMAP4_SSL")
def test_imap_fetch_folder_not_found_returns_404(mock_imap_cls, client):
    """imap.select() returning NO surfaces as 404, not 200 with empty results."""
    mock_imap = MagicMock()
    mock_imap_cls.return_value = mock_imap
    mock_imap.select.return_value = ("NO", [b"folder does not exist"])
    mock_imap.close.return_value = ("OK", [])
    mock_imap.logout.return_value = ("BYE", [])

    resp = client.post("/primitives/imap/fetch", json={
        "server": "imap.test.com", "port": 993,
        "user": "me@test.com", "password": "secret",
        "folder": "DoesNotExist",
    })
    assert resp.status_code == 404
    assert "folder" in resp.json()["detail"].lower()


@patch("primitives.imap.imaplib.IMAP4_SSL")
def test_imap_search_escapes_backslash_and_quote(mock_imap_cls, client):
    """CHARSET UTF-8 path passes raw UTF-8 bytes; query with special chars must appear in the call args."""
    mock_imap = MagicMock()
    mock_imap_cls.return_value = mock_imap
    mock_imap.select.return_value = ("OK", [b"1"])
    mock_imap.uid.return_value = ("OK", [b""])
    mock_imap.close.return_value = ("OK", [])
    mock_imap.logout.return_value = ("BYE", [])

    resp = client.post("/primitives/imap/search", json={
        "server": "imap.test.com", "port": 993,
        "user": "me@test.com", "password": "secret",
        "query": r'foo\bar"baz',
        "limit": 5,
    })

    assert resp.status_code == 200, resp.text
    # With CHARSET UTF-8 path, args are: ("search", "CHARSET", "UTF-8", "TEXT", query_bytes)
    # Verify the primitive attempted UTF-8 first with the raw query bytes (no escaping needed).
    mock_imap.uid.assert_any_call("search", "CHARSET", "UTF-8", "TEXT", b'foo\\bar"baz')


def test_imap_fetch_limit_must_be_at_least_1(client):
    """limit=0 must be rejected by pydantic (prevents Python list[-0:] full-slice quirk)."""
    resp = client.post("/primitives/imap/fetch", json={
        "server": "imap.test.com", "port": 993,
        "user": "me@test.com", "password": "secret",
        "limit": 0,
    })
    assert resp.status_code == 422


@patch("primitives.imap.imaplib.IMAP4_SSL")
def test_imap_search_falls_back_to_ascii_on_charset_error(mock_imap_cls, client):
    """If server rejects CHARSET UTF-8, primitive retries with ASCII-only."""
    import imaplib
    mock_imap = MagicMock()
    mock_imap_cls.return_value = mock_imap
    mock_imap.select.return_value = ("OK", [b"1"])
    mock_imap.close.return_value = ("OK", [])
    mock_imap.logout.return_value = ("BYE", [])

    def _uid_side_effect(cmd, *args):
        if cmd == "search" and len(args) >= 2 and args[0] == "CHARSET":
            # Server rejects CHARSET UTF-8
            raise imaplib.IMAP4.error("unsupported charset")
        if cmd == "search":
            # ASCII fallback
            return ("OK", [b""])
        raise RuntimeError(f"unexpected: {cmd} {args}")
    mock_imap.uid.side_effect = _uid_side_effect

    resp = client.post("/primitives/imap/search", json={
        "server": "imap.test.com", "port": 993,
        "user": "me@test.com", "password": "secret",
        "query": "hello", "limit": 5,
    })

    assert resp.status_code == 200, resp.text
    # Verify both attempts happened: first with CHARSET, then without.
    call_args = [call.args for call in mock_imap.uid.call_args_list]
    assert any("CHARSET" in c for c in call_args), f"CHARSET attempt missing: {call_args}"
