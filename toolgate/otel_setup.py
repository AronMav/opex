"""OpenTelemetry bootstrap for Toolgate.

Activated only when ``OTEL_EXPORTER_OTLP_ENDPOINT`` is set in the environment
— otherwise the module is a no-op and toolgate runs without the OTel
dependency tree loaded. This keeps deploys on hosts that don't have a
collector cheap.

The collector endpoint defaults to ``http://127.0.0.1:4317`` (gRPC OTLP),
matching the Jaeger all-in-one setup wired by
``docker/docker-compose.observability.yml``. Service name is taken from
``OTEL_SERVICE_NAME`` (default ``toolgate``) so the Jaeger UI shows it as a
distinct row alongside ``hydeclaw-core``.

Three things get instrumented:

  * ``FastAPIInstrumentor`` — incoming HTTP routes (every endpoint is now a
    parent span). Trace context arrives via ``traceparent`` header from
    Core and is propagated automatically.
  * ``HTTPXClientInstrumentor`` — outgoing httpx calls to provider backends
    (TTS server, STT server, embedding server). Each provider call becomes
    a child span and the traceparent header is forwarded so the upstream
    LLM/embedding service can join the trace if it's also instrumented.
  * Default global tracer — used by call-sites that need their own span
    via ``tracer = trace.get_tracer(__name__)``.

Why batched span exporter: synchronous export would add ~5-50 ms per
request to every toolgate call, which is unacceptable for hot paths like
embeddings (called inline during memory bootstrap).
"""

import logging
import os

log = logging.getLogger("toolgate.otel")


def init_otel(app=None) -> bool:
    """Initialize OTel exporters + instrumentations. Returns ``True`` if
    instrumentation was activated, ``False`` if disabled or unavailable.

    Idempotent: multiple calls during reload should not double-register
    instrumentations. The two FastAPI/HTTPX instrumentors guard themselves
    via internal singletons.
    """
    endpoint = os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT")
    if not endpoint:
        return False

    try:
        from opentelemetry import trace
        from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import (
            OTLPSpanExporter,
        )
        from opentelemetry.sdk.resources import Resource, SERVICE_NAME
        from opentelemetry.sdk.trace import TracerProvider
        from opentelemetry.sdk.trace.export import BatchSpanProcessor
        from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor
        from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor
    except ImportError as e:
        log.warning("OTEL_EXPORTER_OTLP_ENDPOINT set but opentelemetry "
                    "packages missing (%s); skipping instrumentation", e)
        return False

    service = os.environ.get("OTEL_SERVICE_NAME", "toolgate")
    resource = Resource.create({SERVICE_NAME: service})
    provider = TracerProvider(resource=resource)
    # gRPC default endpoint is http://host:4317; insecure=True because
    # the collector lives on loopback (Pi: 127.0.0.1:4317 via Docker
    # port publish). For TLS-protected collectors set OTEL_EXPORTER_OTLP_INSECURE
    # via env and adjust here.
    exporter = OTLPSpanExporter(endpoint=endpoint, insecure=True)
    provider.add_span_processor(BatchSpanProcessor(exporter))
    trace.set_tracer_provider(provider)

    if app is not None:
        FastAPIInstrumentor.instrument_app(app)
    HTTPXClientInstrumentor().instrument()

    log.info("[otel] toolgate exporting traces to %s as %s", endpoint, service)
    return True
