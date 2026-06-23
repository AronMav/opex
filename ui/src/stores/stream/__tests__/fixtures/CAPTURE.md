# SSE Fixture Capture Notes

Fixtures captured on 2026-04-19 from Pi (opex-core 0.19.5,
MiniMax-M2.7 provider, Arty agent).

## Files

- `short-response.sse` — 1-word answer, no tools, finishes in <2s
- `medium-response.sse` — ~1-sentence answer, truncated at 8s
- `tool-response.sse` — includes `tool-input-*` events (optional)

## Invariants locked in

Each fixture, when fed to the replay harness, must produce:

1. Exactly one `data-session-id` event at the start
2. A sequence of `text-start` / `text-delta` / `text-end` events
3. A terminal `finish` event (or stream-truncation marker for the
   8s-capped case)
4. No parsing errors in the replay test

Regenerating fixtures: re-run the curl commands above when the
SSE wire format changes. Check in both the fixture and the
expected-shape tests together.
