import urllib.request, json, time
import urllib.error

# Start a chat
req_body = json.dumps({
    "agent": "Tyler",
    "messages": [{"role": "user", "content": "Write a very long essay about computing. 2000 words."}],
    "stream": True
}).encode()
r = urllib.request.Request(
    "http://127.0.0.1:18789/api/chat",
    data=req_body,
    headers={
        "Authorization": "Bearer 1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa",
        "Content-Type": "application/json",
        "Accept": "text/event-stream"
    }
)
resp = urllib.request.urlopen(r)
d = json.load(resp)
sid = d["session_id"]
print(f"session: {sid}")

# NO delay ? abort immediately (race with stream registration)
for i in range(3):
    try:
        abort_req = urllib.request.Request(
            f"http://127.0.0.1:18789/api/chat/{sid}/abort?agent=Tyler",
            method="POST",
            headers={"Authorization": "Bearer 1f7f11f73a39dbfec786affe38c18002c3f8a371f9978e5e2122f34cff990eaa"}
        )
        r1 = urllib.request.urlopen(abort_req)
        print(f"abort #{i+1}: {r1.status} {json.load(r1)}")
    except urllib.error.HTTPError as e:
        body = e.read().decode()[:200]
        print(f"abort #{i+1}: HTTP {e.code} {body}")
    time.sleep(0.5)
