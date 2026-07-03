"""Tiny TTL cache for ИТС read/search results."""
import time


class TTLCache:
    def __init__(self, now_fn=time.monotonic, max_items: int = 512):
        self._now = now_fn
        self._max = max_items
        self._data: dict[str, tuple[float, object]] = {}

    def get(self, key: str):
        item = self._data.get(key)
        if item is None:
            return None
        expires_at, val = item
        if self._now() >= expires_at:
            self._data.pop(key, None)
            return None
        return val

    def set(self, key: str, val, ttl_s: float) -> None:
        if len(self._data) >= self._max:
            self._data.pop(next(iter(self._data)), None)  # простая эвикция FIFO
        self._data[key] = (self._now() + ttl_s, val)
