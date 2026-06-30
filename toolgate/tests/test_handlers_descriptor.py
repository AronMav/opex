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


from handlers.descriptor import parse_descriptor

_TRANSCRIBE_SRC = '''\
# <handler>
#   <id>transcribe</id>
#   <label lang="ru">Транскрибировать</label>
#   <label lang="en">Transcribe</label>
#   <description lang="ru">Речь из аудио/видео в текст</description>
#   <description lang="en">Speech from audio/video to text</description>
#   <icon>mic</icon>
#   <match>
#     <mime>audio/*</mime>
#     <mime>video/*</mime>
#     <max_size_mb>200</max_size_mb>
#   </match>
#   <capability>stt</capability>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>

async def run(ctx, file, params):
    return ctx.result.text("hi")
'''


def test_parse_descriptor_happy_path():
    d = parse_descriptor(_TRANSCRIBE_SRC, tier="builtin")
    assert d.id == "transcribe"
    assert d.labels == {"ru": "Транскрибировать", "en": "Transcribe"}
    assert d.descriptions["ru"] == "Речь из аудио/видео в текст"
    assert d.icon == "mic"
    assert d.match_mimes == ["audio/*", "video/*"]
    assert d.max_size_mb == 200
    assert d.capability == "stt"
    assert d.execution == "sync"
    assert d.output == "text"
    assert d.params == [
        {"name": "language", "type": "string", "default": "ru", "required": False}
    ]
    assert d.order == 10
    assert d.enabled is True
    assert d.tier == "builtin"


def test_parse_descriptor_minimal_defaults():
    src = '''\
# <handler>
#   <id>save</id>
#   <label lang="en">Save</label>
#   <icon>save</icon>
#   <match>
#     <mime>*/*</mime>
#   </match>
#   <execution>sync</execution>
# </handler>

async def run(ctx, file, params):
    return ctx.result.text("")
'''
    d = parse_descriptor(src, tier="workspace")
    assert d.id == "save"
    assert d.labels == {"en": "Save"}
    assert d.descriptions == {}
    assert d.match_mimes == ["*/*"]
    assert d.max_size_mb is None
    assert d.capability is None
    assert d.output == "text"  # default when <output> omitted
    assert d.params == []
    assert d.order == 100  # default when <order> omitted
    assert d.enabled is True  # default when <enabled> omitted
    assert d.tier == "workspace"
