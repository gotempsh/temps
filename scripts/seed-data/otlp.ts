// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Minimal OTLP/protobuf encoder for the trace signal.
 *
 * The Temps OTLP ingest endpoint only accepts `application/x-protobuf`
 * (`crates/temps-otel/src/handlers/ingest_handler.rs`), and the full
 * OpenTelemetry JS SDK makes it awkward to emit spans with arbitrary,
 * precisely-controlled start/end timestamps at volume. So this module hand-rolls
 * just the subset of `opentelemetry.proto.collector.trace.v1` we need.
 *
 * Field numbers follow the OTLP proto definitions verbatim — changing one
 * silently produces spans the server decodes as empty.
 */

// ── protobuf wire primitives ────────────────────────────────────────

function varint(value: number | bigint): Uint8Array {
  let v = BigInt(value);
  const out: number[] = [];
  do {
    let byte = Number(v & 0x7fn);
    v >>= 7n;
    if (v > 0n) byte |= 0x80;
    out.push(byte);
  } while (v > 0n);
  return new Uint8Array(out);
}

function tag(fieldNumber: number, wireType: number): Uint8Array {
  return varint((fieldNumber << 3) | wireType);
}

function concat(parts: Uint8Array[]): Uint8Array {
  let total = 0;
  for (const p of parts) total += p.length;
  const out = new Uint8Array(total);
  let offset = 0;
  for (const p of parts) {
    out.set(p, offset);
    offset += p.length;
  }
  return out;
}

/** Length-delimited field (wire type 2): strings, bytes, nested messages. */
function lenField(fieldNumber: number, payload: Uint8Array): Uint8Array {
  return concat([tag(fieldNumber, 2), varint(payload.length), payload]);
}

function strField(fieldNumber: number, value: string): Uint8Array {
  return lenField(fieldNumber, new TextEncoder().encode(value));
}

function varintField(fieldNumber: number, value: number | bigint): Uint8Array {
  return concat([tag(fieldNumber, 0), varint(value)]);
}

/** fixed64 field (wire type 1), little-endian — used for the nano timestamps. */
function fixed64Field(fieldNumber: number, value: bigint): Uint8Array {
  const buf = new Uint8Array(8);
  new DataView(buf.buffer).setBigUint64(0, value, true);
  return concat([tag(fieldNumber, 1), buf]);
}

function doubleField(fieldNumber: number, value: number): Uint8Array {
  const buf = new Uint8Array(8);
  new DataView(buf.buffer).setFloat64(0, value, true);
  return concat([tag(fieldNumber, 1), buf]);
}

// ── OTLP messages ───────────────────────────────────────────────────

export type AttrValue = string | number | boolean;

/** `opentelemetry.proto.common.v1.AnyValue` */
function anyValue(value: AttrValue): Uint8Array {
  if (typeof value === "string") return strField(1, value);
  if (typeof value === "boolean") return varintField(2, value ? 1 : 0);
  if (Number.isInteger(value)) return varintField(3, value);
  return doubleField(4, value);
}

/** `opentelemetry.proto.common.v1.KeyValue` */
function keyValue(key: string, value: AttrValue): Uint8Array {
  return concat([strField(1, key), lenField(2, anyValue(value))]);
}

function attributes(attrs: Record<string, AttrValue>, fieldNumber: number): Uint8Array[] {
  return Object.entries(attrs).map(([k, v]) => lenField(fieldNumber, keyValue(k, v)));
}

export const SpanKind = {
  UNSPECIFIED: 0,
  INTERNAL: 1,
  SERVER: 2,
  CLIENT: 3,
  PRODUCER: 4,
  CONSUMER: 5,
} as const;

export const StatusCode = {
  UNSET: 0,
  OK: 1,
  ERROR: 2,
} as const;

export interface SeedSpan {
  traceId: string; // 32 hex chars
  spanId: string; // 16 hex chars
  parentSpanId?: string;
  name: string;
  kind: number;
  startTimeUnixNano: bigint;
  endTimeUnixNano: bigint;
  attributes: Record<string, AttrValue>;
  statusCode: number;
  statusMessage?: string;
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/** `opentelemetry.proto.trace.v1.Span` */
function encodeSpan(span: SeedSpan): Uint8Array {
  const parts: Uint8Array[] = [
    lenField(1, hexToBytes(span.traceId)),
    lenField(2, hexToBytes(span.spanId)),
  ];
  if (span.parentSpanId) parts.push(lenField(4, hexToBytes(span.parentSpanId)));
  parts.push(strField(5, span.name));
  parts.push(varintField(6, span.kind));
  parts.push(fixed64Field(7, span.startTimeUnixNano));
  parts.push(fixed64Field(8, span.endTimeUnixNano));
  parts.push(...attributes(span.attributes, 9));

  // Status is field 15; message is field 2 inside it, code field 3.
  const status: Uint8Array[] = [];
  if (span.statusMessage) status.push(strField(2, span.statusMessage));
  status.push(varintField(3, span.statusCode));
  parts.push(lenField(15, concat(status)));

  return concat(parts);
}

export interface ResourceSpanGroup {
  resourceAttributes: Record<string, AttrValue>;
  scopeName: string;
  spans: SeedSpan[];
}

/**
 * Encode a full `ExportTraceServiceRequest`, ready to POST to
 * `/otel/v1/traces` with `Content-Type: application/x-protobuf`.
 */
export function encodeExportTraceServiceRequest(groups: ResourceSpanGroup[]): Uint8Array {
  const resourceSpans = groups.map((group) => {
    // Resource { attributes = 1 }
    const resource = lenField(1, concat(attributes(group.resourceAttributes, 1)));
    // InstrumentationScope { name = 1 }
    const scope = lenField(1, strField(1, group.scopeName));
    const spans = group.spans.map((s) => lenField(2, encodeSpan(s)));
    // ScopeSpans { scope = 1, spans = 2 }
    const scopeSpans = lenField(2, concat([scope, ...spans]));
    // ResourceSpans { resource = 1, scope_spans = 2 }
    return lenField(1, concat([resource, scopeSpans]));
  });
  return concat(resourceSpans);
}

// ── id helpers ──────────────────────────────────────────────────────

const HEX = "0123456789abcdef";

export function randomHex(chars: number): string {
  const bytes = new Uint8Array(chars / 2);
  crypto.getRandomValues(bytes);
  let out = "";
  for (const b of bytes) out += HEX.charAt(b >> 4) + HEX.charAt(b & 0x0f);
  return out;
}

export const newTraceId = () => randomHex(32);
export const newSpanId = () => randomHex(16);
