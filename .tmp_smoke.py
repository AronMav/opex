import urllib.request, json, time
time.sleep(5)
# /api/doctor/tools ? ????? agent_misuse ??????????
req = urllib.request.Request(
    "http://127.0.0.1:18789/api/doctor/tools",
    headers={"Authorization": "Bearer 1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa"}
)
d = json.load(urllib.request.urlopen(req))
print("total degraded:", d["degraded_count"])
for cat in ["code_fixed", "agent_misuse", "config_needed", "operator_service", "unknown"]:
    items = d["categories"].get(cat, [])
    print(f"  {cat}: {len(items)}")
# Health
req = urllib.request.Request(
    "http://127.0.0.1:18789/api/doctor",
    headers={"Authorization": "Bearer 1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa"}
)
d = json.load(urllib.request.urlopen(req))
print(f"overall ok: {d['ok']}")
