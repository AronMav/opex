"""Unit tests for normalize.py signature changes (NormalizeLLMConfig)."""

import re

import pytest
import respx
import httpx

from normalize import (
    NormalizeLLMConfig,
    normalize_via_llm,
    normalize_text,
    transliterate_latin,
    _ipa_to_cyrillic,
)


def test_ipa_to_cyrillic_mapping():
    # Deterministic IPA → Cyrillic (no espeak needed).
    assert _ipa_to_cyrillic("klˈɔːd") == "клод"          # Claude
    assert _ipa_to_cyrillic("ænθɹˈɑːpɪk") == "энтрапик"  # Anthropic
    assert _ipa_to_cyrillic("dʒˈɛmᵻnˌaɪ") == "джэминай"  # Gemini (constellation)


def test_brand_dictionary_overrides_g2p():
    # Curated names win over espeak's literal pronunciation.
    assert transliterate_latin("Claude") == "клод"
    assert transliterate_latin("Gemini") == "джемини"
    assert transliterate_latin("Qwen") == "квен"


def test_transliterate_dict_word():
    assert transliterate_latin("Python") == "пайтон"
    assert transliterate_latin("GPU") == "джи пи ю"


def test_transliterate_unknown_acronym_spelled_out():
    # Not in dict, all-caps ≤5 letters → spelled letter by letter.
    assert transliterate_latin("XYZ") == "экс уай зет"


def test_transliterate_leaves_no_latin():
    out = transliterate_latin("Запусти server через API и Docker")
    assert not re.search(r"[A-Za-z]", out)
    assert "сервер" in out


@pytest.mark.asyncio
async def test_normalize_text_transliterates_english(http_client):
    """English words must come out as Cyrillic, never silently dropped."""
    out = await normalize_text(http_client, "Открой Python и проверь API", config=None)
    assert "пайтон" in out.lower()
    assert "эй пи ай" in out.lower()
    assert not re.search(r"[A-Za-z]", out)


@pytest.mark.asyncio
async def test_normalize_via_llm_with_none_config_returns_none(http_client):
    """When config is None, skip LLM step entirely — return None."""
    result = await normalize_via_llm(http_client, "Hello Python", config=None)
    assert result is None


@pytest.mark.asyncio
async def test_normalize_via_llm_with_valid_config_calls_http(http_client):
    """With a valid config, a POST is made to config.base_url."""
    config = NormalizeLLMConfig(
        base_url="http://llm-test/v1/chat/completions",
        api_key="test-key",
        model="test-model",
    )
    async with respx.mock(assert_all_called=True) as mock:
        mock.post("http://llm-test/v1/chat/completions").mock(
            return_value=httpx.Response(
                200,
                json={"choices": [{"message": {"content": "Привет Пайтон"}}]},
            )
        )
        result = await normalize_via_llm(http_client, "Hello Python", config=config)
    assert result == "Привет Пайтон"


@pytest.mark.asyncio
async def test_normalize_via_llm_skips_when_no_latin_chars(http_client):
    """Short-circuit: no Latin chars → no LLM call, returns None."""
    config = NormalizeLLMConfig(base_url="http://unused", api_key="k", model="m")
    result = await normalize_via_llm(http_client, "Только кириллица", config=config)
    assert result is None


@pytest.mark.asyncio
async def test_normalize_text_without_config_runs_pre_post_only(http_client):
    """normalize_text with config=None does pre/post processing only."""
    result = await normalize_text(http_client, "Тест 123 руб.", config=None)
    # Numbers expanded, abbreviation expanded
    assert "сто двадцать три" in result
    assert "рублей" in result


@pytest.mark.asyncio
async def test_normalize_via_llm_skips_when_api_key_empty(http_client):
    """Config with empty api_key → no LLM call, returns None (same as config=None path).
    Guards against a misconfigured provider registry entry silently passing through."""
    config = NormalizeLLMConfig(base_url="http://unused", api_key="", model="m")
    result = await normalize_via_llm(http_client, "Hello Python", config=config)
    assert result is None
