import asyncio
import pytest
from profiles import ProfileManager

class FakeCtx:
    def __init__(self, user_data_dir): self.user_data_dir = user_data_dir; self.closed = False
    async def close(self): self.closed = True

@pytest.mark.asyncio
async def test_same_profile_returns_singleton():
    made = []
    async def factory(udd): c = FakeCtx(udd); made.append(c); return c
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    a = await pm.get_context("its")
    b = await pm.get_context("its")
    assert a is b                      # singleton на профиль
    assert len(made) == 1
    assert a.user_data_dir == "/tmp/profiles/its"

@pytest.mark.asyncio
async def test_different_profiles_isolated():
    async def factory(udd): return FakeCtx(udd)
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    a = await pm.get_context("its")
    b = await pm.get_context("other")
    assert a is not b

@pytest.mark.asyncio
async def test_concurrent_get_creates_one():
    made = []
    async def factory(udd):
        await asyncio.sleep(0.01); c = FakeCtx(udd); made.append(c); return c
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    ctxs = await asyncio.gather(*[pm.get_context("its") for _ in range(5)])
    assert len({id(c) for c in ctxs}) == 1   # гонка не плодит контексты
    assert len(made) == 1

@pytest.mark.asyncio
async def test_close_all():
    async def factory(udd): return FakeCtx(udd)
    pm = ProfileManager(factory=factory, root="/tmp/profiles")
    c = await pm.get_context("its")
    await pm.close_all()
    assert c.closed is True
