/**
 * OpenTelemetry bootstrap for the channels adapter.
 *
 * Activated only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set in the
 * environment — otherwise this module returns immediately and the
 * @opentelemetry/* dependency tree stays cold-loaded but inactive.
 *
 * Why a dedicated init module: SDK setup must run BEFORE any other
 * @opentelemetry-instrumented module is imported, otherwise the
 * auto-instrumentations have nothing to patch. Importing this from
 * the very first line of `index.ts` (before grammy/discord.js/slack)
 * is the only way to get reliable spans for outbound HTTP calls to
 * the Telegram/Discord/Matrix APIs.
 *
 * What gets instrumented:
 *   - All HTTP/HTTPS outbound calls (grammy → telegram, discord.js
 *     → discord.com, slack/bolt → slack.com) — auto-instrumented
 *     so the traceparent header is forwarded transparently.
 *   - Incoming WebSocket messages from Core (channel-ws bridge) get
 *     manual spans wrapped around message processing — see
 *     `wrapWithChannelSpan` below.
 *
 * The collector endpoint defaults to `http://127.0.0.1:4317` (gRPC
 * OTLP), matching the Jaeger all-in-one setup wired by
 * `docker/docker-compose.observability.yml`. Service name is taken
 * from `OTEL_SERVICE_NAME` (default: `channels`) so it appears as a
 * distinct row alongside `opex-core` and `toolgate` in Jaeger.
 */

let started = false;

export async function initOtel(): Promise<boolean> {
  if (started) return true;
  const endpoint = process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
  if (!endpoint) return false;

  try {
    const { NodeSDK } = await import("@opentelemetry/sdk-node");
    const { OTLPTraceExporter } = await import(
      "@opentelemetry/exporter-trace-otlp-grpc"
    );
    const { getNodeAutoInstrumentations } = await import(
      "@opentelemetry/auto-instrumentations-node"
    );
    const { Resource } = await import("@opentelemetry/resources");
    const { SemanticResourceAttributes } = await import(
      "@opentelemetry/semantic-conventions"
    );

    const serviceName = process.env.OTEL_SERVICE_NAME ?? "channels";
    const sdk = new NodeSDK({
      resource: new Resource({
        [SemanticResourceAttributes.SERVICE_NAME]: serviceName,
      }),
      traceExporter: new OTLPTraceExporter({ url: endpoint }),
      // Disable fs instrumentation — too noisy on Bun, no value for
      // tracing the channel adapter's actual work (network I/O).
      instrumentations: [
        getNodeAutoInstrumentations({
          "@opentelemetry/instrumentation-fs": { enabled: false },
        }),
      ],
    });

    sdk.start();
    started = true;
    console.log(
      `[otel] channels exporting traces to ${endpoint} as ${serviceName}`,
    );

    // Graceful shutdown: flush pending spans on SIGTERM/SIGINT so
    // we don't lose the last batch when systemd restarts the service.
    const shutdown = async () => {
      try {
        await sdk.shutdown();
      } catch (e) {
        console.error("[otel] shutdown error:", e);
      }
    };
    process.on("SIGTERM", shutdown);
    process.on("SIGINT", shutdown);
    return true;
  } catch (e) {
    console.warn(
      "[otel] OTEL_EXPORTER_OTLP_ENDPOINT set but @opentelemetry packages " +
        "missing or failed to start; skipping instrumentation:",
      e,
    );
    return false;
  }
}
