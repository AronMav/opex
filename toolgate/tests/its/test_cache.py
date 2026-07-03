from its.cache import TTLCache

def test_get_before_and_after_expiry():
    t = {"v": 1000.0}
    c = TTLCache(now_fn=lambda: t["v"])
    c.set("k", "val", ttl_s=60)
    assert c.get("k") == "val"
    t["v"] = 1000.0 + 61
    assert c.get("k") is None   # протух

def test_missing_key():
    c = TTLCache(now_fn=lambda: 0.0)
    assert c.get("nope") is None
