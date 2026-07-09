"""F015 regression: the runner's /complete POST must check the HTTP status and
retry on a transient non-2xx instead of silently discarding a completed job's
result (which would leave the handler_jobs row wedged in 'processing').
"""
import asyncio

import pytest

from handlers import runner


class _Resp:
    def __init__(self, code: int):
        self.status_code = code


class _FlakyClient:
    """Returns the queued status codes in order, recording every POST."""

    def __init__(self, codes):
        self.codes = list(codes)
        self.calls = 0

    async def post(self, _url, headers=None, json=None, timeout=None):
        self.calls += 1
        code = self.codes.pop(0) if self.codes else 200
        return _Resp(code)


@pytest.fixture(autouse=True)
def _no_backoff_sleep(monkeypatch):
    async def _instant(*_a, **_k):
        return None
    monkeypatch.setattr(runner.asyncio, "sleep", _instant)


@pytest.mark.asyncio
async def test_post_complete_retries_until_2xx():
    client = _FlakyClient([500, 503, 200])
    await runner._post_complete(client, "http://core", "job1", {}, {"status": "done"})
    assert client.calls == 3, "must retry transient 5xx and succeed on the 2xx"


@pytest.mark.asyncio
async def test_post_complete_gives_up_on_auth_rejection():
    client = _FlakyClient([401, 200])
    await runner._post_complete(client, "http://core", "job1", {}, {"status": "done"})
    assert client.calls == 1, "an expired-token 401 won't recover — must not retry"


@pytest.mark.asyncio
async def test_post_complete_bounded_by_max_retries():
    # Always fails: must stop after _COMPLETE_MAX_RETRIES, not loop forever.
    client = _FlakyClient([500] * 20)
    await runner._post_complete(client, "http://core", "job1", {}, {"status": "done"})
    assert client.calls == runner._COMPLETE_MAX_RETRIES
