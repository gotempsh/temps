// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import { signalFixture } from './ingest-setup.fixture'
import {
  analyticsBrowserSnippet,
  analyticsCurl,
  errorsCurl,
  externalOtelEnvBlock,
  otelCollectorConfig,
  otlpCurl,
  otlpEnvVars,
  otlpLogsRequest,
  otlpTracesRequest,
  sentryEnvelope,
} from './plain-http-setup'

type Field = { field: number; wire: number; value: bigint | Uint8Array }

/** A minimal protobuf reader: enough to check the encoder's output. */
function parse(bytes: Uint8Array): Field[] {
  const out: Field[] = []
  let i = 0
  const varint = () => {
    let shift = 0n
    let v = 0n
    for (;;) {
      const b = bytes[i++]
      v |= BigInt(b & 0x7f) << shift
      if (!(b & 0x80)) return v
      shift += 7n
    }
  }
  while (i < bytes.length) {
    const key = Number(varint())
    const field = key >> 3
    const wire = key & 7
    if (wire === 0) out.push({ field, wire, value: varint() })
    else if (wire === 1) {
      let v = 0n
      for (let k = 0n; k < 8n; k++) v |= BigInt(bytes[i++]) << (8n * k)
      out.push({ field, wire, value: v })
    } else if (wire === 2) {
      const len = Number(varint())
      out.push({ field, wire, value: bytes.slice(i, i + len) })
      i += len
    } else throw new Error(`unexpected wire type ${wire}`)
  }
  return out
}
const sub = (fields: Field[], n: number) =>
  parse(fields.find((f) => f.field === n)?.value as Uint8Array)
const text = (fields: Field[], n: number) =>
  new TextDecoder().decode(
    fields.find((f) => f.field === n)?.value as Uint8Array
  )
const num = (fields: Field[], n: number) =>
  fields.find((f) => f.field === n)?.value as bigint

const NOW = 1_795_000_000_000

describe('OTLP protobuf encoding', () => {
  test('a logs request carries the service name, body and timestamp', () => {
    const resourceLogs = sub(parse(otlpLogsRequest('my-app', NOW)), 1)
    const attr = sub(sub(sub(resourceLogs, 1), 1), 1)
    expect(text(attr, 1)).toBe('service.name')
    expect(text(sub(attr, 2), 1)).toBe('my-app')
    const record = sub(sub(resourceLogs, 2), 2)
    expect(num(record, 1)).toBe(BigInt(NOW) * 1_000_000n)
    expect(num(record, 2)).toBe(9n)
    expect(text(record, 3)).toBe('INFO')
    expect(text(sub(record, 5), 1)).toContain('Test log')
  })

  test('a traces request carries 16/8-byte ids and a positive duration', () => {
    const traceId = '0123456789abcdef0123456789abcdef'
    const spanId = '0123456789abcdef'
    const resourceSpans = sub(
      parse(otlpTracesRequest('my-app', NOW, traceId, spanId)),
      1
    )
    const span = sub(sub(resourceSpans, 2), 2)
    expect((span.find((f) => f.field === 1)?.value as Uint8Array).length).toBe(
      16
    )
    expect((span.find((f) => f.field === 2)?.value as Uint8Array).length).toBe(
      8
    )
    expect(text(span, 5)).toBe('temps setup check')
    expect(num(span, 8) - num(span, 7)).toBe(25_000_000n)
    expect(num(span, 8)).toBe(BigInt(NOW) * 1_000_000n)
  })
})

describe('curl examples', () => {
  test('analytics posts JSON with the key header template', () => {
    const curl = analyticsCurl(signalFixture('analytics'))
    expect(curl).toContain("'https://temps.example.com/api/_temps/event'")
    expect(curl).toContain('-H "X-Temps-Analytics-Key: $TEMPS_ANALYTICS_KEY"')
    expect(curl).toContain('"event_name":"page_view"')
    expect(curl).toContain('"request_query":""')
  })

  test('the browser snippet uses fetch and a keyed text/plain beacon', () => {
    const snippet = analyticsBrowserSnippet(signalFixture('analytics'))
    expect(snippet).toContain("'X-Temps-Analytics-Key': TEMPS_KEY")
    expect(snippet).toContain('navigator.sendBeacon')
    expect(snippet).toContain('?temps_key=')
    expect(snippet).toContain("type: 'text/plain'")
  })

  test('errors post a Sentry envelope keyed from $SENTRY_DSN', () => {
    const curl = errorsCurl(signalFixture('errors'), NOW)
    expect(curl).toContain('"$SENTRY_DSN"')
    expect(curl).toContain(
      'https://temps.example.com/api/42/envelope/?sentry_key=$SENTRY_KEY'
    )
    expect(curl).toContain('application/x-sentry-envelope')
    const lines = sentryEnvelope(NOW, 'a'.repeat(32)).split('\n')
    expect(lines).toHaveLength(3)
    expect(JSON.parse(lines[1])).toEqual({ type: 'event' })
    expect(JSON.parse(lines[2]).event_id).toBe('a'.repeat(32))
  })

  test('OTLP posts protobuf with the bearer and project headers', () => {
    for (const signal of ['logs', 'traces'] as const) {
      const curl = otlpCurl(signalFixture(signal), 'my-app', NOW)
      expect(curl).toContain(
        `'https://temps.example.com/api/otel/v1/${signal}'`
      )
      expect(curl).toContain('-H "Authorization: Bearer $TEMPS_API_KEY"')
      expect(curl).toContain('-H "X-Temps-Project-Id: 42"')
      expect(curl).toContain('application/x-protobuf')
      expect(curl).toContain('base64 --decode')
    }
  })

  test('OTel SDK variables URL-encode the bearer space', () => {
    const vars = otlpEnvVars(signalFixture('traces'), 'my-app')
    expect(vars).toContain(
      'OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=https://temps.example.com/api/otel/v1/traces'
    )
    expect(vars).toContain('Authorization=Bearer%20$TEMPS_API_KEY')
    expect(vars).toContain('OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf')
  })

  test('the Collector reads the API key from its environment', () => {
    const config = otelCollectorConfig(signalFixture('logs'))
    expect(config).toContain(
      'logs_endpoint: https://temps.example.com/api/otel/v1/logs'
    )
    expect(config).toContain('${env:TEMPS_API_KEY}')
    expect(config).toContain('encoding: proto')
  })
})

test('the external .env block never puts a path-based URL in OTEL_EXPORTER_OTLP_ENDPOINT', () => {
  // SDKs append /v1/traces to OTEL_EXPORTER_OTLP_ENDPOINT, so the old
  // `.../api/otel/v1/{project}/{env}/0` value exported to a 404.
  const setup = signalFixture('traces')
  const pathBased = {
    ...setup,
    endpoints: [
      {
        ...setup.endpoints[0],
        url: 'https://temps.example.com/api/otel/v1/42/7/0/traces',
      },
    ],
  }
  const block = externalOtelEnvBlock(pathBased, 'my-app')
  expect(block).not.toMatch(/^OTEL_EXPORTER_OTLP_ENDPOINT=/m)
  expect(block).toContain(
    'OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=https://temps.example.com/api/otel/v1/42/7/0/traces'
  )
  expect(block).not.toContain('<YOUR_API_KEY>')
  expect(externalOtelEnvBlock(undefined, 'my-app')).toContain('Loading')
})
