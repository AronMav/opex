"""Generic named persistent browser profiles. Site-agnostic."""
import asyncio
import os
import posixpath
import re

_PROFILE_RE = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")


class ProfileManager:
    def __init__(self, factory, root: str | None = None):
        # factory: async (user_data_dir: str) -> BrowserContext
        self._factory = factory
        self._root = root or os.environ.get("PROFILES_DIR", "/profiles")
        self._contexts: dict[str, object] = {}
        self._lock = asyncio.Lock()

    def profiles_root(self) -> str:
        return self._root

    async def get_context(self, profile: str):
        if not _PROFILE_RE.fullmatch(profile):
            raise ValueError(f"invalid profile name: {profile!r}")
        existing = self._contexts.get(profile)
        if existing is not None:
            return existing
        async with self._lock:
            existing = self._contexts.get(profile)   # double-checked под локом
            if existing is not None:
                return existing
            udd = posixpath.join(self._root, profile)
            os.makedirs(udd, exist_ok=True)
            ctx = await self._factory(udd)
            self._contexts[profile] = ctx
            return ctx

    async def close_all(self) -> None:
        for ctx in list(self._contexts.values()):
            try:
                await ctx.close()
            except Exception:
                pass
        self._contexts.clear()
