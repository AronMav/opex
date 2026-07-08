"""Version-aware OpenAI path joining (mirrors core's registry.rs::join_openai_path)."""

from providers.base import join_openai_path


def test_versioned_base_drops_v1():
    # Base already carries a version segment → the /v1 is dropped.
    assert (
        join_openai_path("https://api.z.ai/api/coding/paas/v4", "/v1/embeddings")
        == "https://api.z.ai/api/coding/paas/v4/embeddings"
    )
    assert (
        join_openai_path("https://openrouter.ai/api/v1", "/v1/audio/speech")
        == "https://openrouter.ai/api/v1/audio/speech"
    )


def test_root_base_keeps_v1():
    # Root base (no version) → /v1 is added.
    assert (
        join_openai_path("https://api.openai.com", "/v1/audio/transcriptions")
        == "https://api.openai.com/v1/audio/transcriptions"
    )


def test_trailing_slash_and_noop_for_active_configs():
    assert join_openai_path("http://10.8.0.2:8000/v1/", "/v1/audio/transcriptions") == (
        "http://10.8.0.2:8000/v1/audio/transcriptions"
    )
    # Non-/v1 suffix is appended verbatim regardless of version.
    assert join_openai_path("https://x.com/v4", "/models") == "https://x.com/v4/models"
