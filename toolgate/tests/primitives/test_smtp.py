"""Unit tests for /primitives/smtp/send."""

from unittest.mock import MagicMock, patch

import pytest
import smtplib
from fastapi.testclient import TestClient


@pytest.fixture
def client():
    from fastapi import FastAPI
    from primitives import smtp

    app = FastAPI()
    app.include_router(smtp.router)
    return TestClient(app)


def test_smtp_send_rejects_missing_fields(client):
    resp = client.post("/primitives/smtp/send", json={})
    assert resp.status_code == 422


@patch("primitives.smtp.smtplib.SMTP")
def test_smtp_send_happy_path(mock_smtp_cls, client):
    mock_smtp = MagicMock()
    mock_smtp_cls.return_value.__enter__.return_value = mock_smtp

    resp = client.post("/primitives/smtp/send", json={
        "server": "smtp.test.com",
        "port": 587,
        "user": "me@test.com",
        "password": "secret",
        "to": "you@test.com",
        "subject": "hi",
        "body": "hello world",
        "html": False,
    })

    assert resp.status_code == 200, resp.text
    data = resp.json()
    assert data["status"] == "sent"

    mock_smtp_cls.assert_called_once_with("smtp.test.com", 587, timeout=15)
    mock_smtp.starttls.assert_called_once()
    mock_smtp.login.assert_called_once_with("me@test.com", "secret")
    mock_smtp.sendmail.assert_called_once()


@patch("primitives.smtp.smtplib.SMTP")
def test_smtp_send_auth_failure_returns_401(mock_smtp_cls, client):
    mock_smtp = MagicMock()
    mock_smtp_cls.return_value.__enter__.return_value = mock_smtp
    mock_smtp.login.side_effect = smtplib.SMTPAuthenticationError(535, b"bad creds")

    resp = client.post("/primitives/smtp/send", json={
        "server": "s", "port": 587, "user": "u", "password": "p",
        "to": "x@y.com", "subject": "s", "body": "b",
    })
    assert resp.status_code == 401


@patch("primitives.smtp.smtplib.SMTP")
def test_smtp_send_html_uses_multipart(mock_smtp_cls, client):
    mock_smtp = MagicMock()
    mock_smtp_cls.return_value.__enter__.return_value = mock_smtp

    resp = client.post("/primitives/smtp/send", json={
        "server": "s", "port": 587, "user": "u", "password": "p",
        "to": "x@y.com", "subject": "s", "body": "<b>bold</b>",
        "html": True,
    })
    assert resp.status_code == 200
    # sendmail was called with a multipart/alternative message
    sent_args = mock_smtp.sendmail.call_args
    raw_message = sent_args[0][2]
    assert "multipart/alternative" in raw_message
