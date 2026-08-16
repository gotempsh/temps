/**
 * Minimal hand-rolled OTLP/HTTP protobuf encoder.
 *
 * Builds real `ExportTraceServiceRequest` / `ExportMetricsServiceRequest` /
 * `ExportLogsServiceRequest` payloads matching the exact wire schema vendored
 * at `crates/temps-otel/proto/opentelemetry/proto/**` (field numbers below are
 * copied from those `.proto` files, not guessed/assumed) -- these are the
 * same messages `temps-otel`'s `ingest/decode.rs` decodes with `prost`.
 *
 * No `@opentelemetry/*` SDK dependency: encoding by hand gives byte-exact
 * control over payload size, which the quota-enforcement test needs (to push
 * a precise, large volume of trace data past a configured quota) and which a
 * real SDK's own batching/export internals would fight.
 */

// ── Low-level protobuf writer ───────────────────────────────────────

const WIRE_VARINT = 0
const WIRE_FIXED64 = 1
const WIRE_LEN = 2
const WIRE_FIXED32 = 5

const utf8 = new TextEncoder()

class ByteWriter {
  private buf: Uint8Array
  private len = 0

  constructor(initial = 256) {
    this.buf = new Uint8Array(initial)
  }

  private ensure(extra: number): void {
    if (this.len + extra <= this.buf.length) return
    let cap = this.buf.length * 2
    while (cap < this.len + extra) cap *= 2
    const next = new Uint8Array(cap)
    next.set(this.buf.subarray(0, this.len))
    this.buf = next
  }

  writeByte(b: number): void {
    this.ensure(1)
    this.buf[this.len++] = b & 0xff
  }

  writeRaw(bytes: Uint8Array): void {
    this.ensure(bytes.length)
    this.buf.set(bytes, this.len)
    this.len += bytes.length
  }

  /** Unsigned varint (protobuf's base128 LEB encoding). */
  writeVarint(value: number): void {
    if (value < 0 || !Number.isFinite(value)) {
      throw new Error(`writeVarint: value must be a non-negative finite number, got ${value}`)
    }
    let v = Math.floor(value)
    this.ensure(10)
    while (v >= 0x80) {
      this.buf[this.len++] = (v & 0x7f) | 0x80
      v = Math.floor(v / 128)
    }
    this.buf[this.len++] = v & 0x7f
  }

  private writeTag(field: number, wireType: number): void {
    this.writeVarint((field << 3) | wireType)
  }

  varintField(field: number, value: number): void {
    this.writeTag(field, WIRE_VARINT)
    this.writeVarint(value)
  }

  boolField(field: number, value: boolean): void {
    this.varintField(field, value ? 1 : 0)
  }

  stringField(field: number, value: string): void {
    const bytes = utf8.encode(value)
    this.writeTag(field, WIRE_LEN)
    this.writeVarint(bytes.length)
    this.writeRaw(bytes)
  }

  bytesField(field: number, value: Uint8Array): void {
    this.writeTag(field, WIRE_LEN)
    this.writeVarint(value.length)
    this.writeRaw(value)
  }

  /** Embed an already-encoded sub-message (length-delimited, same wire type as bytes). */
  messageField(field: number, value: Uint8Array): void {
    this.bytesField(field, value)
  }

  /** `fixed64` — 8 raw little-endian bytes carrying a `bigint`. */
  fixed64Field(field: number, value: bigint): void {
    this.writeTag(field, WIRE_FIXED64)
    this.ensure(8)
    const view = new DataView(this.buf.buffer, this.buf.byteOffset + this.len, 8)
    view.setBigUint64(0, value, true)
    this.len += 8
  }

  /** `double` — IEEE754 8-byte little-endian. */
  doubleField(field: number, value: number): void {
    this.writeTag(field, WIRE_FIXED64)
    this.ensure(8)
    const view = new DataView(this.buf.buffer, this.buf.byteOffset + this.len, 8)
    view.setFloat64(0, value, true)
    this.len += 8
  }

  /** `fixed32` — 4 raw little-endian bytes. */
  fixed32Field(field: number, value: number): void {
    this.writeTag(field, WIRE_FIXED32)
    this.ensure(4)
    const view = new DataView(this.buf.buffer, this.buf.byteOffset + this.len, 4)
    view.setUint32(0, value >>> 0, true)
    this.len += 4
  }

  finish(): Uint8Array {
    return this.buf.subarray(0, this.len)
  }
}

// ── OTLP common types (opentelemetry/proto/common/v1/common.proto) ──

export type AttributeValue = string | number | boolean

/** `AnyValue { oneof value { string_value=1, bool_value=2, int_value=3, double_value=4 } }`. */
function encodeAnyValue(value: AttributeValue): Uint8Array {
  const w = new ByteWriter(64)
  if (typeof value === 'string') w.stringField(1, value)
  else if (typeof value === 'boolean') w.boolField(2, value)
  else if (Number.isInteger(value)) w.varintField(3, value)
  else w.doubleField(4, value)
  return w.finish()
}

/** `KeyValue { key=1 string, value=2 AnyValue }`. */
function encodeKeyValue(key: string, value: AttributeValue): Uint8Array {
  const w = new ByteWriter(64 + key.length)
  w.stringField(1, key)
  w.messageField(2, encodeAnyValue(value))
  return w.finish()
}

function writeAttributes(
  w: ByteWriter,
  field: number,
  attrs: Record<string, AttributeValue> | undefined,
): void {
  if (!attrs) return
  for (const [k, v] of Object.entries(attrs)) {
    w.messageField(field, encodeKeyValue(k, v))
  }
}

/** `Resource { attributes=1 repeated KeyValue }` (opentelemetry/proto/resource/v1/resource.proto). */
function encodeResource(attrs: Record<string, AttributeValue>): Uint8Array {
  const w = new ByteWriter(256)
  writeAttributes(w, 1, attrs)
  return w.finish()
}

/** Build the default resource attributes every OTLP payload in this file carries. */
export function defaultResourceAttrs(serviceName: string, extra?: Record<string, AttributeValue>): Record<string, AttributeValue> {
  return { 'service.name': serviceName, ...extra }
}

// ── IDs ──────────────────────────────────────────────────────────────

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2)
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.substr(i * 2, 2), 16)
  return out
}

function randomHex(bytes: number): string {
  const arr = new Uint8Array(bytes)
  crypto.getRandomValues(arr)
  return Array.from(arr, (b) => b.toString(16).padStart(2, '0')).join('')
}

/** A fresh 32-lowercase-hex-char trace id (16 random bytes), matching OTLP's `is_valid_trace_id`. */
export function randomTraceId(): string {
  return randomHex(16)
}

/** A fresh 16-lowercase-hex-char span id (8 random bytes). */
export function randomSpanId(): string {
  return randomHex(8)
}

/**
 * A high-entropy ASCII string of exactly `byteLength` bytes, built from
 * random bytes via base64 (trimmed/repeated to the exact length).
 *
 * NOT a repeated/low-entropy filler ('x'.repeat(n)): Postgres TOASTs large
 * column values through pglz compression, which crushes a repeated-byte
 * string down to a few KB regardless of `byteLength` -- a volume test using
 * one would send real bytes over the wire but store almost nothing, making
 * `hypertable_size()`-based quota math look under-counted (or a quota look
 * uncrossable) for a reason that has nothing to do with the quota code
 * itself. Random bytes are effectively incompressible, so what's sent is
 * (approximately) what lands on disk -- the volume this function's callers
 * actually need to simulate a real flood.
 */
export function randomFillerAscii(byteLength: number): string {
  // base64 emits 4 output chars per 3 input bytes -- generate enough random
  // bytes to cover `byteLength` output chars, then trim to the exact size.
  const neededRandomBytes = Math.ceil((byteLength * 3) / 4) + 3
  const raw = new Uint8Array(neededRandomBytes)
  // crypto.getRandomValues has a real per-call limit (64KiB on most
  // implementations) -- fill in fixed-size chunks rather than one giant call.
  const CHUNK = 65536
  for (let offset = 0; offset < raw.length; offset += CHUNK) {
    crypto.getRandomValues(raw.subarray(offset, Math.min(offset + CHUNK, raw.length)))
  }
  return Buffer.from(raw).toString('base64').slice(0, byteLength)
}

function nowUnixNano(offsetMs = 0): bigint {
  return BigInt(Date.now() + offsetMs) * 1_000_000n
}

// ── Traces (opentelemetry/proto/trace/v1/trace.proto) ───────────────

export interface SpanInput {
  traceId: string
  spanId: string
  /** Empty/omitted = root span (no parent) -- required for the span to appear as a trace root. */
  parentSpanId?: string
  name: string
  /** SpanKind enum value; 1 = INTERNAL (default). */
  kind?: number
  startUnixNano?: bigint
  endUnixNano?: bigint
  attributes?: Record<string, AttributeValue>
  /** Status.StatusCode; 0=UNSET, 1=OK, 2=ERROR. */
  statusCode?: number
}

/** `Span` message. */
function encodeSpan(s: SpanInput): Uint8Array {
  const w = new ByteWriter(256)
  w.bytesField(1, hexToBytes(s.traceId))
  w.bytesField(2, hexToBytes(s.spanId))
  if (s.parentSpanId) w.bytesField(4, hexToBytes(s.parentSpanId))
  w.stringField(5, s.name)
  w.varintField(6, s.kind ?? 1)
  w.fixed64Field(7, s.startUnixNano ?? nowUnixNano(-10))
  w.fixed64Field(8, s.endUnixNano ?? nowUnixNano())
  writeAttributes(w, 9, s.attributes)
  if (s.statusCode !== undefined) {
    const status = new ByteWriter(8)
    status.varintField(3, s.statusCode)
    w.messageField(15, status.finish())
  }
  return w.finish()
}

/** Build a full `ExportTraceServiceRequest` for one resource/scope with N spans. */
export function buildTraceExportRequest(opts: {
  resourceAttrs: Record<string, AttributeValue>
  spans: SpanInput[]
}): Uint8Array {
  const scopeSpans = new ByteWriter(1024)
  for (const s of opts.spans) scopeSpans.messageField(2, encodeSpan(s))

  const resourceSpans = new ByteWriter(1024)
  resourceSpans.messageField(1, encodeResource(opts.resourceAttrs))
  resourceSpans.messageField(2, scopeSpans.finish())

  const req = new ByteWriter(1024)
  req.messageField(1, resourceSpans.finish())
  return req.finish()
}

// ── Metrics (opentelemetry/proto/metrics/v1/metrics.proto) ──────────

export interface NumberDataPointInput {
  timeUnixNano?: bigint
  asDouble?: number
  asInt?: bigint
  attributes?: Record<string, AttributeValue>
}

function encodeNumberDataPoint(p: NumberDataPointInput): Uint8Array {
  const w = new ByteWriter(128)
  writeAttributes(w, 7, p.attributes)
  w.fixed64Field(3, p.timeUnixNano ?? nowUnixNano())
  if (p.asInt !== undefined) w.fixed64Field(6, p.asInt) // sfixed64, same wire encoding as fixed64
  else w.doubleField(4, p.asDouble ?? 0)
  return w.finish()
}

export interface GaugeMetricInput {
  name: string
  unit?: string
  dataPoints: NumberDataPointInput[]
}

function encodeGaugeMetric(m: GaugeMetricInput): Uint8Array {
  const gauge = new ByteWriter(256)
  for (const dp of m.dataPoints) gauge.messageField(1, encodeNumberDataPoint(dp))

  const w = new ByteWriter(256)
  w.stringField(1, m.name)
  if (m.unit) w.stringField(3, m.unit)
  w.messageField(5, gauge.finish()) // oneof data { gauge = 5 }
  return w.finish()
}

/** Build a full `ExportMetricsServiceRequest` for one resource/scope with N gauge metrics. */
export function buildMetricsExportRequest(opts: {
  resourceAttrs: Record<string, AttributeValue>
  metrics: GaugeMetricInput[]
}): Uint8Array {
  const scopeMetrics = new ByteWriter(1024)
  for (const m of opts.metrics) scopeMetrics.messageField(2, encodeGaugeMetric(m))

  const resourceMetrics = new ByteWriter(1024)
  resourceMetrics.messageField(1, encodeResource(opts.resourceAttrs))
  resourceMetrics.messageField(2, scopeMetrics.finish())

  const req = new ByteWriter(1024)
  req.messageField(1, resourceMetrics.finish())
  return req.finish()
}

// ── Logs (opentelemetry/proto/logs/v1/logs.proto) ────────────────────

export interface LogRecordInput {
  timeUnixNano?: bigint
  /** SeverityNumber enum; 9 = INFO. */
  severityNumber?: number
  severityText?: string
  body: string
  attributes?: Record<string, AttributeValue>
  traceId?: string
  spanId?: string
}

function encodeLogRecord(r: LogRecordInput): Uint8Array {
  const w = new ByteWriter(256)
  w.fixed64Field(1, r.timeUnixNano ?? nowUnixNano())
  w.varintField(2, r.severityNumber ?? 9)
  w.stringField(3, r.severityText ?? 'INFO')
  w.messageField(5, encodeAnyValue(r.body))
  writeAttributes(w, 6, r.attributes)
  if (r.traceId) w.bytesField(9, hexToBytes(r.traceId))
  if (r.spanId) w.bytesField(10, hexToBytes(r.spanId))
  return w.finish()
}

/** Build a full `ExportLogsServiceRequest` for one resource/scope with N log records. */
export function buildLogsExportRequest(opts: {
  resourceAttrs: Record<string, AttributeValue>
  records: LogRecordInput[]
}): Uint8Array {
  const scopeLogs = new ByteWriter(1024)
  for (const r of opts.records) scopeLogs.messageField(2, encodeLogRecord(r))

  const resourceLogs = new ByteWriter(1024)
  resourceLogs.messageField(1, encodeResource(opts.resourceAttrs))
  resourceLogs.messageField(2, scopeLogs.finish())

  const req = new ByteWriter(1024)
  req.messageField(1, resourceLogs.finish())
  return req.finish()
}

// ── Sender ────────────────────────────────────────────────────────────

export interface OtlpSendResult {
  status: number
  ok: boolean
  bodyText: string
}

/**
 * POST a raw OTLP protobuf payload to `${apiBaseUrl}/otel/v1/{signal}` using
 * `tk_` API-key header auth (`Authorization` + `X-Temps-Project-Id`) -- the
 * same auth path a real collector/SDK configured with a project API key
 * would use. Not routed through the generated SDK client: its default
 * `bodySerializer` is `JSON.stringify`, which would corrupt a raw protobuf
 * `Uint8Array` body.
 */
export async function sendOtlp(opts: {
  apiBaseUrl: string
  apiKey: string
  projectId: number
  signal: 'traces' | 'metrics' | 'logs'
  payload: Uint8Array
}): Promise<OtlpSendResult> {
  const res = await fetch(`${opts.apiBaseUrl}/otel/v1/${opts.signal}`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${opts.apiKey}`,
      'X-Temps-Project-Id': String(opts.projectId),
      'Content-Type': 'application/x-protobuf',
    },
    // Uint8Array is a valid runtime BodyInit (fetch accepts any ArrayBufferView),
    // but this repo's configured lib/target doesn't type it that way -- same
    // reasoning as node:tls's raw-socket use elsewhere in this package.
    body: opts.payload as BodyInit,
  })
  const bodyText = await res.text().catch(() => '')
  return { status: res.status, ok: res.ok, bodyText }
}
