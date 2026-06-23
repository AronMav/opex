# Observability — OTel + Jaeger Setup

How to enable distributed tracing for OPEX Core on the Pi (or any host)
and how to validate that spans actually arrive at Jaeger under load.

## Prerequisites

- OPEX deployed via the standard `make deploy` workflow.
- Docker daemon running on the host (Pi or local).
- ~512 MB free RAM for Jaeger all-in-one.

## What's Wired

The pipeline is already instrumented at three hot paths:

| Span name | Where | Fields |
| --- | --- | --- |
| `pipeline.execute` | `pipeline/execute.rs` | `session_id`, `agent`, `iterations`, `assistant_message_id` |
| `pipeline.finalize` | `pipeline/finalize.rs` | `session_id`, `agent`, `outcome` (done/failed/interrupted) |
| `pipeline.execute_tools` | `pipeline/parallel.rs` | `session_id`, `tool_count`, `loop_break` |

These spans are emitted via `tracing::instrument` and propagated to OTLP only
when the `otel` feature is built in **and** `[otel] enabled = true` in
`opex.toml`.

## Local Development Workflow

```bash
# 1. Boot Jaeger locally (binds 4317 + 16686 on 127.0.0.1)
docker compose -f docker/docker-compose.observability.yml up -d

# 2. Build core with the otel feature
cargo build --features otel -p opex-core --release

# 3. Run core pointing at the local collector
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 \
  ./target/release/opex-core

# 4. Open Jaeger UI
open http://localhost:16686
```

In `config/opex.toml`, set:

```toml
[otel]
enabled = true
service_name = "opex-core"
```

## Pi Production Workflow

```bash
# 1. Boot Jaeger on Pi
make jaeger-up

# 2. Build + deploy OTel-instrumented binary
make deploy-binary-otel

# 3. Edit /etc/systemd/system/opex-core.service to add:
#    Environment="OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317"
#    (or in ~/opex/.env if loaded by the unit)

# 4. Set [otel] enabled = true in ~/opex/config/opex.toml on Pi

# 5. Restart core to pick up env + config
ssh $PI_HOST "systemctl --user restart opex-core"

# 6. Tunnel + open the Jaeger UI from your laptop
ssh -L 16686:127.0.0.1:16686 $PI_HOST
# Then: http://localhost:16686
```

The convenience target `make deploy-jaeger` chains step 1 + 2 + restart.
Steps 3–4 are one-time configuration and not automated by the Makefile.

## Validating Spans Under Load

A correctly instrumented system should show:

1. **Smoke check (1 request).** Send one chat request and verify the span
   tree appears in Jaeger UI within 5 seconds:
   `pipeline.execute` → multiple `pipeline.execute_tools` (one per tool
   batch) → one `pipeline.finalize`.
2. **Field check.** Click into `pipeline.execute` and confirm
   `iterations` and `assistant_message_id` are populated (these are
   recorded mid-loop and at the end via `Span::current().record(...)`).
3. **Outcome distribution.** Filter `pipeline.finalize` by the `outcome`
   tag — `done` should dominate; `failed` and `interrupted` are visible
   when reproduced (kill the request mid-stream, force a 5xx LLM error).

Synthetic load script — runs the existing chaos test repeatedly to
generate enough spans to detect drops:

```bash
for i in {1..20}; do
  OPEX_AUTH_TOKEN=<token> python3 tests/integration/pi/test-pi-chaos.py &
done
wait
```

After the loop completes, in Jaeger UI:

- Service: `opex-core`
- Operation: `pipeline.execute`
- Lookback: 1h
- Limit: 100

You should see ≥ 20 traces. Each one ends in either `pipeline.finalize`
(outcome=done|failed) or terminates at `pipeline.execute` itself if the
chaos drop happened before finalize fired.

## Performance Notes

- The OTel exporter is **batching** (`with_batch_exporter`) — spans are
  not flushed on every call. Expect ~1–5s latency between span creation
  and visibility in Jaeger.
- On Pi, the `opex-core` binary grows by ~3 MB with the `otel`
  feature enabled (extra dependency: `opentelemetry-otlp` + `tonic`).
- Memory: Jaeger all-in-one is configured for 50k spans in memory
  (~50–100 MB at saturation). Oldest spans evict first.
- Disk: Jaeger all-in-one uses no disk storage — spans are lost on
  container restart. Acceptable for a single-host Pi setup; for
  durable storage swap to `otel-collector` → Tempo or Elasticsearch.

## Troubleshooting

**Spans don't appear in Jaeger UI:**

1. Confirm core was built with `--features otel`:
   `~/opex/opex-core-aarch64 --version 2>&1 | head -1` should
   show `[otel]` in the boot log.
2. Confirm `OTEL_EXPORTER_OTLP_ENDPOINT` env var is set on Pi:
   `systemctl --user show opex-core | grep OTEL`.
3. Confirm Jaeger is up and listening on 4317:
   `ss -tlnp | grep 4317` should show docker-proxy or jaeger-all-in-one.
4. Tail Core logs for `[otel]` boot messages:
   `journalctl --user -u opex-core --no-pager | grep otel | tail`.

**Jaeger UI shows no service:**

- Service name appears only after the first span is exported. Send a
  test request first.

## Architectural Notes

- The collector lives in a separate compose file (`docker-compose.observability.yml`)
  rather than the main `docker-compose.yml` so production deploys don't
  pay the memory cost when observability isn't needed.
- `service.name` in `[otel] service_name` is the only piece of identity
  that surfaces in Jaeger's service dropdown. If you run multiple Core
  instances against the same collector, give each a distinct
  `service_name` (e.g. `opex-core-prod` vs `opex-core-dev`).

### Cross-process tracing (Core ↔ Toolgate ↔ Channels)

All three processes export to the same Jaeger collector and share a
W3C TraceContext propagator, so a `pipeline.execute` parent span on
Core continues into the Toolgate `POST /v1/embeddings` and Channels
`http send` child spans within a single Jaeger trace.

- **Core (outgoing)**: `trace_propagation::inject_trace_context(req)`
  wraps a `reqwest::RequestBuilder` and injects `traceparent` headers
  when the `otel` feature is enabled. Wired into the four Toolgate
  paths: `/v1/embeddings`, `/transcribe` (STT), `/describe` (vision),
  `/web` (URL fetch).
- **Core (incoming)**: `trace_propagation::extract_trace_context_layer`
  is registered as the outermost Axum middleware on the gateway router.
  It pulls `traceparent` from incoming HTTP headers, opens an
  `http_request` span with method+target attributes, and binds the
  upstream parent context. Any downstream span created during request
  processing (`pipeline.execute`, `pipeline.execute_tools`, etc.)
  inherits the upstream trace_id, so a single Jaeger trace covers
  external caller → Core → Toolgate. The existing
  `trace_context_middleware` (logging-correlation only) is preserved
  in parallel — different concern, different scope.
- **Toolgate**: `opentelemetry-instrumentation-fastapi` automatically
  extracts `traceparent` from incoming requests and links new spans
  to that trace. `opentelemetry-instrumentation-httpx` propagates
  the same context onto outgoing calls (e.g. to the embedding
  backend).
- **Channels**: `@opentelemetry/auto-instrumentations-node` patches
  `node:http` so outbound calls (Telegram, Discord, Slack APIs) get
  `traceparent` injected automatically. Inbound WebSocket from Core
  is wrapped manually if you need spans there.

To enable observability for the managed processes on Pi, add these
keys to the `[[managed_process]]` `env_extra` in `opex.toml`:

```toml
# Inline-table form used by opex.toml — service_name is "toolgate"
# for the toolgate process and "channels" for the channels process.
env_extra = { ...,
    OTEL_EXPORTER_OTLP_ENDPOINT = "http://127.0.0.1:4317",
    OTEL_SERVICE_NAME = "toolgate" }
```

Both `toolgate` and `channels` no-op if the env var is absent — same
contract as Core.
