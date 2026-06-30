'use strict';

// Loaded via `node -r ./tracing.js server.js` so the HTTP instrumentation
// patches the `http` module before the app requires it.
//
// Temps auto-injects OTEL_EXPORTER_OTLP_ENDPOINT / _HEADERS / _PROTOCOL and
// OTEL_SERVICE_NAME into every deployed container. In a normal deployment the
// injected endpoint is reachable as-is. In the local dev-cluster the injected
// host is `app.localho.st`, which resolves to loopback *inside* a worker
// container, so we rewrite it to the control plane's reachable cluster address
// (CP_INTERNAL_HOST) over plain http. No-op when CP_INTERNAL_HOST is unset.
const cpHost = process.env.CP_INTERNAL_HOST;
if (cpHost && process.env.OTEL_EXPORTER_OTLP_ENDPOINT) {
  process.env.OTEL_EXPORTER_OTLP_ENDPOINT = process.env.OTEL_EXPORTER_OTLP_ENDPOINT
    .replace('app.localho.st', cpHost)
    .replace('https://', 'http://');
}

const { NodeSDK } = require('@opentelemetry/sdk-node');
const { OTLPTraceExporter } = require('@opentelemetry/exporter-trace-otlp-proto');
const { HttpInstrumentation } = require('@opentelemetry/instrumentation-http');

// OTLPTraceExporter reads OTEL_EXPORTER_OTLP_ENDPOINT + OTEL_EXPORTER_OTLP_HEADERS
// (the project's ingest auth) from the environment. NodeSDK reads
// OTEL_SERVICE_NAME for the resource service.name.
const sdk = new NodeSDK({
  traceExporter: new OTLPTraceExporter(),
  instrumentations: [new HttpInstrumentation()],
});

try {
  sdk.start();
  console.log('otel: tracing started, exporting to', process.env.OTEL_EXPORTER_OTLP_ENDPOINT);
} catch (e) {
  console.error('otel: failed to start:', (e && e.message) || e);
}

process.on('SIGTERM', () => {
  sdk.shutdown().catch(() => {}).finally(() => process.exit(0));
});
