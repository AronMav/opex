"""Execution context for file handlers.

`ctx` is the ONLY sanctioned API a handler sees. Handlers receive the file's
RAW BYTES (R12) — they never fetch a loopback URL (toolgate's SSRF guard hard-
blocks loopback, and core already downloads the upload in Rust and POSTs the
bytes as multipart). Provider wrappers inject the shared httpx.AsyncClient
internally so handlers never touch the client or credentials.

Two http surfaces are exposed:
  - ctx.http_client_raw : the shared httpx.AsyncClient. Used for provider calls
    (the provider Protocols take it as first arg) and direct byte work. NOT
    SSRF-validated because it is only ever pointed at trusted provider backends.
  - ctx.http : an SsrfHttpClient wrapper for handler-initiated EXTERNAL fetches
    (e.g. a workspace handler hitting a public API). Every .get/.post validates
    the URL via the same guard download_limited uses, blocking private /
    link-local hosts.

`ctx.result` builds the ScenarioOutcome wire shape the core consumes
(status/summary_text/artifact_urls/reason) — exactly 4 keys (R9).
"""
from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

import httpx

# Reuse toolgate's existing SSRF URL validator (the same one download_limited
# calls). Imported at module level so tests can monkeypatch ctxmod.validate_url_ssrf.
from helpers import validate_url_ssrf  # noqa: F401  (re-exported for monkeypatching)

log = logging.getLogger("toolgate.handlers")


@dataclass
class HandlerFile:
    """Uploaded file bytes plus metadata passed to a handler's run() function."""

    bytes: bytes
    mime: str
    filename: str
    size: int
    # For url-based handlers (e.g. video) where core sends a source_url form
    # field and no upload bytes. None for normal upload-backed handlers.
    source_url: str | None = None


@dataclass
class HandlerResult:
    """Mirrors the core ScenarioOutcome wire type (snake_case)."""

    status: str = "ok"
    summary_text: str = ""
    artifact_urls: list[str] = field(default_factory=list)
    reason: str | None = None
    # Optional side-channel payload consumed by the file-scenario runner after
    # the job completes (e.g. write an Obsidian note). NOT part of the 4-key
    # ScenarioOutcome wire sent to core — included only when set so the runner
    # can act on it and the core /complete payload stays clean.
    post_action: dict | None = None

    def to_dict(self) -> dict[str, Any]:
        # R9: emit EXACTLY these 4 keys. Core's ScenarioOutcome has a 5th field
        # (video_accepted) with serde default false; it deserializes this fine.
        d: dict[str, Any] = {
            "status": self.status,
            "summary_text": self.summary_text,
            "artifact_urls": list(self.artifact_urls),
            "reason": self.reason,
        }
        if self.post_action is not None:
            d["post_action"] = self.post_action
        return d


class ResultBuilder:
    """Builds a HandlerResult. `.file`/`.card` carry the b64/card payload in
    artifact_urls/summary_text; v1 sync handlers mostly use `.text`."""

    def text(self, s: str) -> HandlerResult:
        return HandlerResult(status="ok", summary_text=s)

    def file(self, data: bytes, mime: str) -> HandlerResult:
        import base64
        b64 = base64.b64encode(data).decode("ascii")
        return HandlerResult(
            status="ok",
            summary_text=f"[file {mime} {len(data)} bytes]",
            artifact_urls=[f"data:{mime};base64,{b64}"],
        )

    def card(self, card_type: str, data: dict) -> HandlerResult:
        import json
        return HandlerResult(
            status="ok",
            summary_text=json.dumps({"card_type": card_type, "data": data}),
        )

    def failed(self, reason: str) -> HandlerResult:
        return HandlerResult(status="failed", reason=reason)

    def unsupported(self, reason: str) -> HandlerResult:
        return HandlerResult(status="unsupported", reason=reason)

    def too_large(self, reason: str) -> HandlerResult:
        return HandlerResult(status="too_large", reason=reason)


class _LlmClient:
    """Thin helper for calling the core raw-LLM endpoint `POST /api/llm/complete`.

    Uses the RAW httpx.AsyncClient (NOT the SSRF-guarded ctx.http) because the
    core URL is a trusted loopback endpoint, exactly like ctx.progress does.
    Constructed by build_context and exposed as ctx.llm.
    """

    def __init__(
        self,
        core_url: str | None,
        auth_token: str | None,
        http: httpx.AsyncClient,
    ):
        self._core_url = core_url
        self._auth_token = auth_token
        self._http = http

    async def complete(
        self,
        messages: list[dict],
        provider: str | None = None,
        model: str | None = None,
    ) -> str:
        """POST `{core_url}/api/llm/complete` and return the `text` field.

        Raises RuntimeError if core_url is unset (sync/in-process context with
        no job runner) or if the response is non-2xx.
        """
        if not self._core_url:
            raise RuntimeError(
                "ctx.llm.complete requires core_url to be set; "
                "this context was built without a job runner"
            )
        url = f"{self._core_url.rstrip('/')}/api/llm/complete"
        headers: dict[str, str] = {}
        if self._auth_token:
            headers["Authorization"] = f"Bearer {self._auth_token}"
        body: dict = {"messages": messages}
        if provider is not None:
            body["provider"] = provider
        if model is not None:
            body["model"] = model
        resp = await self._http.post(url, json=body, headers=headers, timeout=120.0)
        if resp.status_code < 200 or resp.status_code >= 300:
            raise RuntimeError(
                f"LLM complete failed: HTTP {resp.status_code}: {resp.text[:200]}"
            )
        return resp.json()["text"]


class SsrfHttpClient:
    """SSRF-safe facade over the shared httpx.AsyncClient (R5/R12). Every
    .get/.post validates the URL via the same guard download_limited uses, so a
    handler fetching an attacker-influenced URL cannot reach private/link-local
    hosts. Used ONLY for handler-initiated EXTERNAL fetches — provider/byte
    calls use ctx.http_client_raw.

    The validator is called via the module-global name `validate_url_ssrf` (not a
    local reference), so test monkeypatching of `ctxmod.validate_url_ssrf` works
    correctly — Python resolves module-level names at call time.
    """

    def __init__(self, http: httpx.AsyncClient):
        self._http = http

    async def get(self, url: str, **kwargs):
        validate_url_ssrf(url)
        return await self._http.get(url, **kwargs)

    async def post(self, url: str, **kwargs):
        validate_url_ssrf(url)
        return await self._http.post(url, **kwargs)


class _CapabilityWrapper:
    """Resolves the active provider for `capability` per call and injects the
    shared RAW http client as the provider Protocol's first positional
    argument (R12: providers call their own trusted backends).
    """

    def __init__(self, registry, http: httpx.AsyncClient, capability: str):
        self._registry = registry
        self._http = http
        self._capability = capability

    async def _resolve(self):
        provider = await self._registry.aget_active(self._capability)
        if provider is None:
            raise RuntimeError(f"no active {self._capability} provider")
        return provider

    # ── STT ─────────────────────────────────────────────────────────────────
    async def transcribe(
        self,
        audio_bytes: bytes,
        *,
        filename: str = "audio.ogg",
        language: str = "ru",
        model: str | None = None,
    ) -> str:
        p = await self._resolve()
        return await p.transcribe(self._http, audio_bytes, filename, language, model)

    # ── Vision ───────────────────────────────────────────────────────────────
    async def describe(
        self,
        image_bytes: bytes,
        *,
        content_type: str,
        prompt: str,
        max_tokens: int = 2000,
    ) -> str:
        p = await self._resolve()
        return await p.describe(self._http, image_bytes, content_type, prompt, max_tokens)

    # ── TTS ──────────────────────────────────────────────────────────────────
    async def synthesize(
        self,
        text: str,
        *,
        voice: str,
        model: str | None = None,
        response_format: str = "mp3",
    ) -> bytes:
        p = await self._resolve()
        return await p.synthesize(
            self._http, text, voice, model, response_format,
            registry=self._registry,
        )

    # ── ImageGen ─────────────────────────────────────────────────────────────
    async def generate(
        self,
        prompt: str,
        *,
        size: str = "1024x1024",
        model: str | None = None,
        quality: str = "standard",
    ) -> bytes:
        p = await self._resolve()
        return await p.generate(self._http, prompt, size, model, quality)

    # ── Embedding ────────────────────────────────────────────────────────────
    async def embed(
        self,
        texts: list[str],
        *,
        model: str | None = None,
    ) -> list[list[float]]:
        p = await self._resolve()
        return await p.embed(self._http, texts, model)

    # ── WebSearch ────────────────────────────────────────────────────────────
    async def search(
        self,
        query: str,
        *,
        max_results: int = 5,
    ) -> list[dict]:
        p = await self._resolve()
        return await p.search(self._http, query, max_results)


@dataclass
class HandlerContext:
    """All state a handler's run() function is allowed to touch."""

    stt: _CapabilityWrapper
    vision: _CapabilityWrapper
    tts: _CapabilityWrapper
    imagegen: _CapabilityWrapper
    search: _CapabilityWrapper
    embed: _CapabilityWrapper
    # SSRF-safe client for handler-initiated external fetches
    http: SsrfHttpClient
    # Raw shared client for trusted provider/byte calls (not SSRF-validated)
    http_client_raw: httpx.AsyncClient
    result: ResultBuilder
    log: logging.Logger
    # LLM helper for calling core's /api/llm/complete endpoint
    llm: _LlmClient
    # Operator-set per-agent settings (OpenWebUI-style "valves"), keyed by the
    # field names declared in the handler's <config> descriptor block. Empty when
    # nothing is configured; handlers should fall back to their own defaults.
    config: dict = field(default_factory=dict)
    _job_id: str | None = None
    _core_url: str | None = None
    _auth_token: str | None = None
    _registry: object | None = None

    async def progress(self, phase: str, pct: int) -> None:
        """Post progress to the core progress callback when a job_id is set;
        a no-op for sync handlers (no job_id). Uses the RAW client because the
        core callback URL is a trusted loopback endpoint, not handler input.
        """
        if not self._job_id or not self._core_url:
            return
        url = f"{self._core_url.rstrip('/')}/api/files/jobs/{self._job_id}/progress"
        headers: dict[str, str] = {}
        if self._auth_token:
            headers["Authorization"] = f"Bearer {self._auth_token}"
        try:
            await self.http_client_raw.post(
                url,
                json={"phase": phase, "pct": pct},
                headers=headers,
                timeout=10.0,
            )
        except Exception as e:  # progress is best-effort
            self.log.warning("progress callback failed: %s", e)

    async def has_capability(self, capability: str) -> bool:
        """True when an active provider for `capability` is configured.

        Resolve-only probe (no provider call) so handlers can skip an optional
        stage cheaply — e.g. term_fixer skips entirely without websearch,
        BEFORE paying for its detect LLM call.
        """
        if self._registry is None:
            return False
        try:
            return await self._registry.aget_active(capability) is not None
        except Exception:
            return False


def build_context(
    registry,
    http_client: httpx.AsyncClient,
    job_id: str | None = None,
    core_url: str | None = None,
    auth_token: str | None = None,
    config: dict | None = None,
) -> HandlerContext:
    """Construct a HandlerContext for a single handler invocation."""
    return HandlerContext(
        stt=_CapabilityWrapper(registry, http_client, "stt"),
        vision=_CapabilityWrapper(registry, http_client, "vision"),
        tts=_CapabilityWrapper(registry, http_client, "tts"),
        imagegen=_CapabilityWrapper(registry, http_client, "imagegen"),
        search=_CapabilityWrapper(registry, http_client, "websearch"),
        embed=_CapabilityWrapper(registry, http_client, "embedding"),
        http=SsrfHttpClient(http_client),
        http_client_raw=http_client,
        result=ResultBuilder(),
        log=log,
        llm=_LlmClient(core_url, auth_token, http_client),
        config=config or {},
        _job_id=job_id,
        _core_url=core_url,
        _auth_token=auth_token,
        _registry=registry,
    )
