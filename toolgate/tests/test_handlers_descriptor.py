"""Unit tests for toolgate.handlers.descriptor."""

import pytest

from handlers.descriptor import DescriptorError, HandlerDescriptor


def test_descriptor_error_is_exception():
    err = DescriptorError("bad descriptor")
    assert isinstance(err, Exception)
    assert str(err) == "bad descriptor"


def test_handler_descriptor_holds_all_fields():
    d = HandlerDescriptor(
        id="transcribe",
        labels={"ru": "Транскрибировать", "en": "Transcribe"},
        descriptions={"ru": "Речь в текст", "en": "Speech to text"},
        icon="mic",
        match_mimes=["audio/*", "video/*"],
        max_size_mb=200,
        capability="stt",
        execution="sync",
        output="text",
        params=[{"name": "language", "type": "string", "default": "ru", "required": False}],
        order=10,
        enabled=True,
        tier="builtin",
    )
    assert d.id == "transcribe"
    assert d.labels["ru"] == "Транскрибировать"
    assert d.match_mimes == ["audio/*", "video/*"]
    assert d.max_size_mb == 200
    assert d.capability == "stt"
    assert d.execution == "sync"
    assert d.output == "text"
    assert d.params[0]["name"] == "language"
    assert d.order == 10
    assert d.enabled is True
    assert d.tier == "builtin"


def test_handler_descriptor_optional_fields_default_to_none():
    d = HandlerDescriptor(
        id="save",
        labels={"en": "Save"},
        descriptions={},
        icon="save",
        match_mimes=["*/*"],
        max_size_mb=None,
        capability=None,
        execution="sync",
        output="file",
        params=[],
        order=99,
        enabled=True,
        tier="builtin",
    )
    assert d.max_size_mb is None
    assert d.capability is None
    assert d.descriptions == {}
