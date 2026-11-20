// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Framework-agnostic ("Plain HTTP") setup snippets, built from the
 * server-resolved ingest setup (`GET /projects/{id}/ingest-setup`) so every
 * URL is the instance's real public ingress address.
 *
 * Nothing here embeds a credential: snippets read it from the environment
 * variable the setup names (`$TEMPS_ANALYTICS_KEY`, `$SENTRY_DSN`,
 * `$TEMPS_API_KEY`).
 */
import type { IngestEndpoint, IngestSignalSetup } from '@/api/client'

export type IngestSignalName = IngestSignalSetup['signal']

export function endpointUrl(setup: IngestSignalSetup, name: string): string {
  const found = setup.endpoints.find((e: IngestEndpoint) => e.name === name)
  return (found ?? setup.endpoints[0])?.url ?? ''
}

/** Minimal `/_temps/event` payloads: a page view and a custom event. */
export function analyticsPayloads() {
  return {
    page_view: {
      event_name: 'page_view',
      request_path: '/pricing',
      request_query: '',
      event_data: {},
      page_title: 'Pricing',
      domain: 'apps.example.com',
      visitor_id: 'v_0b1c2d3e',
      session_id: 's_4f5a6b7c',
    },
    custom: {
      event_name: 'signup_clicked',
      request_path: '/pricing',
      request_query: '',
      event_data: { plan: 'pro' },
      domain: 'apps.example.com',
      visitor_id: 'v_0b1c2d3e',
      session_id: 's_4f5a6b7c',
    },
  }
}

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`
}

function headerFlags(setup: IngestSignalSetup): string[] {
  return setup.auth.headers.map(
    (h) => `  -H "${h.name}: ${h.value.replace(/"/g, '\\"')}"`
  )
}

/**
 * A key with allowed origins rejects a request without a matching `Origin`
 * (browsers send it; curl and servers do not), so the examples say so.
 */
const ORIGIN_NOTE =
  "# If the key lists allowed origins, add a matching one: -H 'Origin: https://apps.example.com'"

export function analyticsCurl(setup: IngestSignalSetup): string {
  const url = endpointUrl(setup, 'event')
  return [
    ORIGIN_NOTE,
    `curl -i -X POST ${shellQuote(url)} \\`,
    ...headerFlags(setup).map((h) => `${h} \\`),
    `  -H 'Content-Type: application/json' \\`,
    `  -d ${shellQuote(JSON.stringify(analyticsPayloads().page_view))}`,
    '# Expect: HTTP 204',
  ].join('\n')
}

/** Browser snippet: `fetch` for page views and events, `sendBeacon` on unload. */
export function analyticsBrowserSnippet(setup: IngestSignalSetup): string {
  const url = endpointUrl(setup, 'event')
  const param = setup.auth.query_param ?? 'temps_key'
  return `// The analytics key (pa_…) is public by design: it can only write
// analytics for this project. Inject it at build time from
// ${setup.auth.env_var} rather than committing it.
const TEMPS_URL = ${JSON.stringify(url)};
const TEMPS_KEY = '<value of ${setup.auth.env_var}>';

const ids = (() => {
  const get = (k, store) => store.getItem(k) || (store.setItem(k, crypto.randomUUID()), store.getItem(k));
  return { visitor_id: get('temps_vid', localStorage), session_id: get('temps_sid', sessionStorage) };
})();

function track(event_name, event_data = {}) {
  return fetch(TEMPS_URL, {
    method: 'POST',
    keepalive: true,
    headers: { 'Content-Type': 'application/json', 'X-Temps-Analytics-Key': TEMPS_KEY },
    body: JSON.stringify({
      event_name, event_data, ...ids,
      request_path: location.pathname,
      request_query: location.search.replace(/^\\?/, ''),
      page_title: document.title,
      referrer: document.referrer || undefined,
      domain: location.hostname,
      language: navigator.language,
      screen_width: screen.width, screen_height: screen.height,
      viewport_width: innerWidth, viewport_height: innerHeight,
    }),
  });
}

track('page_view');                              // on every page load / route change
// track('signup_clicked', { plan: 'pro' });     // custom event

// Page leave: sendBeacon cannot set headers, so the key goes in ?${param}=.
// A text/plain body avoids a CORS preflight.
addEventListener('pagehide', () => {
  const body = JSON.stringify({ event_name: 'page_leave', event_data: {}, ...ids,
    request_path: location.pathname, request_query: '', domain: location.hostname });
  navigator.sendBeacon(\`\${TEMPS_URL}?${param}=\${encodeURIComponent(TEMPS_KEY)}\`,
    new Blob([body], { type: 'text/plain' }));
});`
}

/**
 * Session replay init. Session IDs are global (a second project reusing one
 * gets a cross-project rejection), so each example gets fresh IDs.
 */
export function sessionReplayCurl(
  setup: IngestSignalSetup,
  sessionId = `s_${hex(8)}`,
  visitorId = `v_${hex(8)}`
): string {
  const url = endpointUrl(setup, 'init')
  const body = JSON.stringify({
    sessionId,
    visitorId,
    url: 'https://apps.example.com/',
  })
  return [
    ORIGIN_NOTE,
    `curl -i -X POST ${shellQuote(url)} \\`,
    ...headerFlags(setup).map((h) => `${h} \\`),
    `  -H 'Content-Type: application/json' \\`,
    `  -d ${shellQuote(body)}`,
    `# Then POST {"sessionId","events","batchId"} to ${endpointUrl(setup, 'events')},`,
    '# where events is base64(zlib(JSON array of rrweb events)).',
  ].join('\n')
}

function hex(bytes: number): string {
  const out = new Uint8Array(bytes)
  crypto.getRandomValues(out)
  return Array.from(out, (b) => b.toString(16).padStart(2, '0')).join('')
}

/** A Sentry envelope holding one error event (newline-delimited JSON). */
export function sentryEnvelope(nowMs: number, eventId = hex(16)): string {
  const timestamp = nowMs / 1000
  return [
    JSON.stringify({
      event_id: eventId,
      sent_at: new Date(nowMs).toISOString(),
    }),
    JSON.stringify({ type: 'event' }),
    JSON.stringify({
      event_id: eventId,
      timestamp,
      platform: 'other',
      level: 'error',
      message: { formatted: 'Test error from the Temps setup page' },
      exception: {
        values: [
          { type: 'Error', value: 'Test error from the Temps setup page' },
        ],
      },
    }),
  ].join('\n')
}

export function errorsCurl(
  setup: IngestSignalSetup,
  nowMs = Date.now()
): string {
  const url = endpointUrl(setup, 'envelope')
  const param = setup.auth.query_param ?? 'sentry_key'
  return [
    `# The DSN looks like https://<public_key>@host/<project_id>.`,
    `SENTRY_KEY=$(printf '%s' "$${setup.auth.env_var}" | sed -E 's#^https?://([^@:]+).*#\\1#')`,
    `printf '%s' ${shellQuote(sentryEnvelope(nowMs))} | \\`,
    `curl -i -X POST "${url}?${param}=$SENTRY_KEY" \\`,
    `  -H 'Content-Type: application/x-sentry-envelope' \\`,
    `  --data-binary @-`,
    '# Expect: HTTP 200 with {"id":"…"}',
  ].join('\n')
}

// ── OTLP/HTTP protobuf ───────────────────────────────────────────────
// Temps' OTLP routes accept protobuf only, so the curl examples carry a
// minimal ExportLogsServiceRequest / ExportTraceServiceRequest, encoded here
// with the current time so the test signal shows up in recent data.

function varint(value: number | bigint): number[] {
  let v = BigInt(value)
  const out: number[] = []
  while (v > 0x7fn) {
    out.push(Number((v & 0x7fn) | 0x80n))
    v >>= 7n
  }
  out.push(Number(v))
  return out
}

const enc = new TextEncoder()
const tag = (field: number, wire: number) => varint((field << 3) | wire)
const bytesField = (field: number, data: number[] | Uint8Array) => [
  ...tag(field, 2),
  ...varint(data.length),
  ...data,
]
const stringField = (field: number, value: string) =>
  bytesField(field, enc.encode(value))
const varintField = (field: number, value: number) => [
  ...tag(field, 0),
  ...varint(value),
]
function fixed64Field(field: number, value: bigint): number[] {
  const out = [...tag(field, 1)]
  for (let i = 0n; i < 8n; i++) out.push(Number((value >> (8n * i)) & 0xffn))
  return out
}
/** `KeyValue { key = 1, value = 2: AnyValue { string_value = 1 } }`. */
const keyValue = (key: string, value: string) => [
  ...stringField(1, key),
  ...bytesField(2, stringField(1, value)),
]
/** `ResourceLogs/ResourceSpans.resource = 1: Resource { attributes = 1 }`. */
const resource = (serviceName: string) =>
  bytesField(1, bytesField(1, keyValue('service.name', serviceName)))
const scope = bytesField(1, stringField(1, 'temps-setup-check'))
const hexBytes = (value: string) =>
  Uint8Array.from(value.match(/../g) ?? [], (b) => parseInt(b, 16))

export function otlpLogsRequest(
  serviceName: string,
  nowMs: number
): Uint8Array {
  const nanos = BigInt(nowMs) * 1_000_000n
  const record = [
    ...fixed64Field(1, nanos),
    ...varintField(2, 9), // SEVERITY_NUMBER_INFO
    ...stringField(3, 'INFO'),
    ...bytesField(5, stringField(1, 'Test log from the Temps setup page')),
    ...fixed64Field(11, nanos),
  ]
  const scopeLogs = [...scope, ...bytesField(2, record)]
  return Uint8Array.from(
    bytesField(1, [...resource(serviceName), ...bytesField(2, scopeLogs)])
  )
}

export function otlpTracesRequest(
  serviceName: string,
  nowMs: number,
  traceId = hex(16),
  spanId = hex(8)
): Uint8Array {
  const end = BigInt(nowMs) * 1_000_000n
  const span = [
    ...bytesField(1, hexBytes(traceId)),
    ...bytesField(2, hexBytes(spanId)),
    ...stringField(5, 'temps setup check'),
    ...varintField(6, 2), // SPAN_KIND_SERVER
    ...fixed64Field(7, end - 25_000_000n),
    ...fixed64Field(8, end),
    ...bytesField(15, varintField(3, 1)), // STATUS_CODE_OK
  ]
  const scopeSpans = [...scope, ...bytesField(2, span)]
  return Uint8Array.from(
    bytesField(1, [...resource(serviceName), ...bytesField(2, scopeSpans)])
  )
}

function toBase64(bytes: Uint8Array): string {
  let binary = ''
  bytes.forEach((b) => (binary += String.fromCharCode(b)))
  return btoa(binary)
}

export function otlpCurl(
  setup: IngestSignalSetup,
  serviceName: string,
  nowMs = Date.now()
): string {
  const isLogs = setup.signal === 'logs'
  const url = endpointUrl(setup, isLogs ? 'otlp_logs' : 'otlp_traces')
  const body = isLogs
    ? otlpLogsRequest(serviceName, nowMs)
    : otlpTracesRequest(serviceName, nowMs)
  return [
    `# One ${isLogs ? 'log record' : 'span'} as OTLP protobuf (OTLP/JSON is not accepted).`,
    `echo '${toBase64(body)}' | base64 --decode | \\`,
    `curl -i -X POST ${shellQuote(url)} \\`,
    ...headerFlags(setup).map((h) => `${h} \\`),
    `  -H 'Content-Type: application/x-protobuf' \\`,
    `  --data-binary @-`,
    '# Expect: HTTP 200',
  ].join('\n')
}

/** `OTEL_*` variables for any OpenTelemetry SDK or the OTel Collector. */
export function otlpEnvVars(
  setup: IngestSignalSetup,
  serviceName: string
): string {
  const signalVar =
    setup.signal === 'logs'
      ? 'OTEL_EXPORTER_OTLP_LOGS_ENDPOINT'
      : 'OTEL_EXPORTER_OTLP_TRACES_ENDPOINT'
  const url = endpointUrl(
    setup,
    setup.signal === 'logs' ? 'otlp_logs' : 'otlp_traces'
  )
  // Header-list values are URL-encoded: the space after Bearer is %20.
  // SDKs and most .env loaders (Node's --env-file, dotenv) do not expand
  // `$VAR`, so the key is a placeholder to paste over, never `$TEMPS_API_KEY`.
  const envRef = `$${setup.auth.env_var}`
  const placeholder = `<value of ${setup.auth.env_var}>`
  const headers = setup.auth.headers
    .map(
      (h) =>
        `${h.name}=${h.value.replace(envRef, placeholder).replace(' ', '%20')}`
    )
    .join(',')
  return [
    `# Paste the key over ${placeholder}: $VARS are not expanded here.`,
    `${signalVar}=${url}`,
    `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`,
    `OTEL_EXPORTER_OTLP_HEADERS=${headers}`,
    `OTEL_SERVICE_NAME=${serviceName}`,
  ].join('\n')
}

/**
 * The `.env` block the OpenTelemetry setup shows for apps Temps does not run.
 * Uses the signal-specific endpoint: SDKs append `/v1/traces` to
 * `OTEL_EXPORTER_OTLP_ENDPOINT`, so a path-based URL there would 404.
 */
export function externalOtelEnvBlock(
  setup: IngestSignalSetup,
  serviceName: string
): string {
  return `# Auto-injected on Temps deployments.
# Set these manually when running on Vercel, Fly, AWS, bare metal, etc.
${otlpEnvVars(setup, serviceName)}`
}

/** An OTel Collector `otlphttp` exporter pointed at Temps. */
export function otelCollectorConfig(setup: IngestSignalSetup): string {
  const logs = setup.signal === 'logs'
  const url = endpointUrl(setup, logs ? 'otlp_logs' : 'otlp_traces')
  const headerLines = setup.auth.headers
    .map(
      (h) =>
        `      ${h.name}: "${h.value.replace('$TEMPS_API_KEY', '${env:TEMPS_API_KEY}')}"`
    )
    .join('\n')
  return `receivers:
  otlp:
    protocols:
      grpc: {}
      http: {}
exporters:
  otlphttp/temps:
    ${logs ? 'logs_endpoint' : 'traces_endpoint'}: ${url}
    encoding: proto
    headers:
${headerLines}
service:
  pipelines:
    ${logs ? 'logs' : 'traces'}:
      receivers: [otlp]
      exporters: [otlphttp/temps]`
}

/** The verification `curl` for a signal. */
export function verificationCurl(
  setup: IngestSignalSetup,
  serviceName: string,
  nowMs = Date.now()
): string {
  switch (setup.signal) {
    case 'analytics':
      return analyticsCurl(setup)
    case 'session_replay':
      return sessionReplayCurl(setup)
    case 'errors':
      return errorsCurl(setup, nowMs)
    case 'logs':
    case 'traces':
      return otlpCurl(setup, serviceName, nowMs)
  }
}
