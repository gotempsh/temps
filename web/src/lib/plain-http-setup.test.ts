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
  sentrySdkExamples,
  sessionReplayCurl,
  sessionReplayExamples,
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

/**
 * `service.name` of a ResourceLogs/ResourceSpans, read by the OTLP schema:
 * `.resource = 1: Resource`, `Resource.attributes = 1: KeyValue`,
 * `KeyValue { key = 1, value = 2: AnyValue { string_value = 1 } }`.
 */
function serviceName(resourceX: Field[]): string | undefined {
  const attributes = sub(sub(resourceX, 1), 1)
  if (text(attributes, 1) !== 'service.name') return undefined
  return text(sub(attributes, 2), 1)
}

const NOW = 1_795_000_000_000

describe('OTLP protobuf encoding', () => {
  test('a logs request carries the service name, body and timestamp', () => {
    // ExportLogsServiceRequest.resource_logs = 1
    const resourceLogs = sub(parse(otlpLogsRequest('my-app', NOW)), 1)
    expect(serviceName(resourceLogs)).toBe('my-app')
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
    expect(serviceName(resourceSpans)).toBe('my-app')
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

  test('session replay inits with camelCase fields and names the batch fields', () => {
    const curl = sessionReplayCurl(
      signalFixture('session_replay'),
      's_0123abcd',
      'v_4567ef01'
    )
    expect(curl).toContain('"sessionId":"s_0123abcd"')
    expect(curl).toContain('"visitorId":"v_4567ef01"')
    expect(curl).toContain('POST {"sessionId","events","batchId"}')
    expect(curl).not.toContain('session_id')
  })

  test('each session replay example gets fresh IDs', () => {
    // Session IDs are global: a second project reusing one is rejected.
    const setup = signalFixture('session_replay')
    const ids = (curl: string) => curl.match(/"(s|v)_[0-9a-f]{16}"/g)
    const first = ids(sessionReplayCurl(setup))
    const second = ids(sessionReplayCurl(setup))
    expect(first).toHaveLength(2)
    expect(second).toHaveLength(2)
    expect(first).not.toEqual(second)
  })

  test('keyed examples say how to pass an allowed origin', () => {
    // A key with allowed origins rejects a request without a matching Origin.
    for (const curl of [
      analyticsCurl(signalFixture('analytics')),
      sessionReplayCurl(signalFixture('session_replay')),
    ]) {
      expect(curl).toContain(
        "# If the key lists allowed origins, add a matching one: -H 'Origin: https://apps.example.com'"
      )
    }
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
    // SDKs and .env loaders do not expand $VARS: a literal `$TEMPS_API_KEY`
    // would be sent as the bearer token.
    expect(vars).toContain(
      'OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<value of TEMPS_API_KEY>,X-Temps-Project-Id=42'
    )
    expect(vars).not.toContain('$TEMPS_API_KEY')
    expect(vars).toContain('OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf')
  })

  test('the Collector reads the API key from its environment', () => {
    const config = otelCollectorConfig(signalFixture('logs'))
    expect(config).toContain(
      'logs_endpoint: https://temps.example.com/api/otel/v1/logs'
    )
    expect(config).toContain('${env:TEMPS_API_KEY}')
    expect(config).toContain('encoding: proto')
    // The pipeline's receiver is declared, so the config loads as is.
    expect(config.startsWith('receivers:\n  otlp:\n    protocols:')).toBe(true)
    expect(config).toContain('receivers: [otlp]')
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
})

describe('library examples', () => {
  test('every Sentry SDK example reads the DSN variable and embeds no DSN', () => {
    const examples = sentrySdkExamples(signalFixture('errors'))
    expect(examples.map((e) => e.id)).toEqual([
      'node',
      'browser',
      'python',
      'go',
      'ruby',
    ])
    for (const example of examples) {
      expect(example.code).toContain('SENTRY_DSN')
      expect(example.code).not.toMatch(/https?:\/\/[^\s'"]*@/)
      expect(example.file.length).toBeGreaterThan(0)
    }
  })

  test('session replay examples point at this project and mask inputs', () => {
    const [react, rrweb] = sessionReplayExamples(
      signalFixture('session_replay')
    )
    expect(react.code).toContain(
      'basePath="https://temps.example.com/api/_temps"'
    )
    expect(react.code).toContain('enableSessionRecording')
    expect(react.code).toContain('maskAllInputs: true')
    expect(rrweb.code).toContain(
      '"https://temps.example.com/api/_temps/session-replay/init"'
    )
    expect(rrweb.code).toContain(
      '"https://temps.example.com/api/_temps/session-replay/events"'
    )
    expect(rrweb.code).toContain("'X-Temps-Analytics-Key'")
    expect(rrweb.code).toContain('maskAllInputs: true')
    expect(rrweb.code).toContain('<value of TEMPS_ANALYTICS_KEY>')
  })

  test('the rrweb example packs events as base64 zlib, which the server inflates', async () => {
    const { inflateSync } = await import('node:zlib')
    const code = sessionReplayExamples(signalFixture('session_replay'))[1].code
    const body = code.match(
      /async function pack\(events\) \{([\s\S]*?)\n\}/
    )?.[1]
    expect(body).toBeTruthy()
    const pack = new Function('events', `return (async () => {${body}})()`) as (
      events: unknown[]
    ) => Promise<string>
    const events = [{ type: 4, data: { href: 'https://apps.example.com/' } }]
    const packed = await pack(events)
    const inflated = inflateSync(Buffer.from(packed, 'base64')).toString()
    expect(JSON.parse(inflated)).toEqual(events)
  })
})

test('the rrweb example keeps a batch until the server accepts it, without keepalive', async () => {
  const { inflateSync } = await import('node:zlib')
  const code = sessionReplayExamples(signalFixture('session_replay'))[1]
    .code.replace(/^import .*$/m, '')
    .replace('export async function', 'async function')
  let emit: (event: unknown) => void = () => {}
  let tick: () => Promise<void> = async () => {}
  const requests: { url: string; init: RequestInit }[] = []
  const answers = [201, 503, 200, 200] // init, failed batch, its retry, next batch
  const fetchStub = async (url: string, init: RequestInit) => {
    requests.push({ url, init })
    const status = answers.shift() ?? 200
    return { ok: status < 300, status }
  }
  const startReplay = new Function(
    'record',
    'fetch',
    'setInterval',
    'sessionStorage',
    'localStorage',
    'location',
    'navigator',
    'innerWidth',
    'innerHeight',
    `${code}\nreturn startReplay;`
  )(
    (options: { emit: (event: unknown) => void }) => {
      emit = options.emit
    },
    fetchStub,
    (fn: () => Promise<void>) => {
      tick = fn
    },
    {},
    {},
    { href: 'https://apps.example.com/' },
    { userAgent: 'test', language: 'en' },
    1280,
    800
  ) as () => Promise<void>

  await startReplay()
  emit({ type: 4, n: 1 })
  await tick() // 503: the batch stays
  emit({ type: 3, n: 2 })
  await tick() // retry: same batch, same id
  await tick() // the event recorded meanwhile goes in a new batch

  const batches = requests
    .filter((r) => r.url.endsWith('/session-replay/events'))
    .map((r) => {
      expect(r.init.keepalive).toBeUndefined()
      const body = JSON.parse(String(r.init.body))
      return {
        batchId: body.batchId,
        events: JSON.parse(
          inflateSync(Buffer.from(body.events, 'base64')).toString()
        ),
      }
    })
  expect(batches).toHaveLength(3)
  expect(batches[1]).toEqual(batches[0])
  expect(batches[0].events).toEqual([{ type: 4, n: 1 }])
  expect(batches[2].batchId).not.toBe(batches[0].batchId)
  expect(batches[2].events).toEqual([{ type: 3, n: 2 }])
})

test('the rrweb example stops recording once failed uploads queue too many events', async () => {
  const code = sessionReplayExamples(signalFixture('session_replay'))[1]
    .code.replace(/^import .*$/m, '')
    .replace('export async function', 'async function')
  const limit = Number(code.match(/MAX_QUEUED_EVENTS = (\d+)/)?.[1])
  expect(limit).toBeGreaterThan(0)
  let emit: (event: unknown) => void = () => {}
  let tick: () => Promise<void> = async () => {}
  let stopped = 0
  let initDone = false
  const startReplay = new Function(
    'record',
    'fetch',
    'setInterval',
    'sessionStorage',
    'localStorage',
    'location',
    'navigator',
    'innerWidth',
    'innerHeight',
    `${code}\nreturn startReplay;`
  )(
    (options: { emit: (event: unknown) => void }) => {
      emit = options.emit
      return () => {
        stopped += 1
      }
    },
    async () => {
      // Init succeeds; every upload after it is refused (an outage).
      const status = initDone ? 503 : 201
      initDone = true
      return { ok: status < 300, status }
    },
    (fn: () => Promise<void>) => {
      tick = fn
    },
    {},
    {},
    { href: 'https://apps.example.com/' },
    { userAgent: 'test', language: 'en' },
    1280,
    800
  ) as () => Promise<void>

  await startReplay()
  emit({ type: 2 })
  await tick() // the first batch fails and stays pending
  // The first event is in the pending batch; the buffer holds the rest.
  for (let i = 0; i < limit - 1; i++) emit({ type: 3, i })
  expect(stopped).toBe(0)
  emit({ type: 3, i: 'last' })
  expect(stopped).toBe(1)
})
