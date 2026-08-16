/**
 * Replay a captured trace into a local Temps instance, verbatim.
 *
 * Takes the JSON body of `GET /otel/global/traces/{trace_id}` (the cross-project
 * unified trace) and pushes each span back through OTLP ingest, mapping the
 * original project ids onto local ones. The point is to reproduce a reported
 * rendering bug against the exact data that produced it — including the
 * millisecond-truncated start times, which is usually where the bug lives.
 *
 * Usage:
 *   TEMPS_API=http://localhost:8261 TEMPS_PASSWORD=... \
 *     bun run ingest-trace-json.ts <trace.json> <origProjectId>=<localId> ...
 */

import {
  encodeExportTraceServiceRequest,
  SpanKind,
  StatusCode,
  type AttrValue,
  type ResourceSpanGroup,
  type SeedSpan,
} from "./otlp.ts";
import { mintApiKey } from "./admin-key.ts";

const API = process.env.TEMPS_API || "http://localhost:8080";
const EMAIL = process.env.TEMPS_EMAIL || "dev@temps.sh";
const PASSWORD = process.env.TEMPS_PASSWORD;

const [tracePath, ...mappingArgs] = process.argv.slice(2);
if (!PASSWORD || !tracePath || mappingArgs.length === 0) {
  console.error(
    "usage: TEMPS_PASSWORD=... bun run ingest-trace-json.ts <trace.json> <orig>=<local> ...",
  );
  process.exit(1);
}

const projectMap = new Map<number, number>(
  mappingArgs.map((pair) => {
    const [from, to] = pair.split("=");
    return [Number(from), Number(to)] as const;
  }),
);

interface CapturedSpan {
  project_id: number;
  span: {
    trace_id: string;
    span_id: string;
    parent_span_id: string | null;
    name: string;
    kind: string;
    start_time: string;
    end_time: string;
    duration_ms: number;
    status_code: string;
    status_message: string;
    attributes: Record<string, string>;
    resource: { service_name: string; service_version: string | null };
  };
}

const captured = (await Bun.file(tracePath).json()) as {
  trace_id: string;
  spans: CapturedSpan[];
};

// ── auth ────────────────────────────────────────────────────────────

let sessionCookie = "";

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(`${API}/api${path}`, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(sessionCookie ? { Cookie: sessionCookie } : {}),
      ...(init.headers || {}),
    },
  });
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`${init.method || "GET"} ${path} -> ${res.status}: ${text.slice(0, 400)}`);
  }
  return (text ? JSON.parse(text) : undefined) as T;
}

const login = await fetch(`${API}/api/auth/login`, {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({ email: EMAIL, password: PASSWORD }),
});
if (!login.ok) throw new Error(`login -> ${login.status}`);
const session = (login.headers.getSetCookie?.() ?? [])
  .map((c) => c.split(";")[0]!)
  .find((c) => c.startsWith("session=") && c !== "session=");
if (!session) throw new Error("login returned no session cookie");
sessionCookie = session;

// Same real MFA + step-up flow the seeder uses — `POST /api-keys` is a guarded
// sensitive action, and a replay script is not a reason to route around it.
const apiKey = await mintApiKey(api, `trace-replay-${Date.now()}`);
console.log("✓ minted ingest key (MFA enrolled, used, removed)");

// ── replay ──────────────────────────────────────────────────────────

const KINDS: Record<string, number> = {
  UNSPECIFIED: SpanKind.UNSPECIFIED,
  INTERNAL: SpanKind.INTERNAL,
  SERVER: SpanKind.SERVER,
  CLIENT: SpanKind.CLIENT,
  PRODUCER: SpanKind.PRODUCER,
  CONSUMER: SpanKind.CONSUMER,
};
const STATUSES: Record<string, number> = {
  UNSET: StatusCode.UNSET,
  OK: StatusCode.OK,
  ERROR: StatusCode.ERROR,
};

/**
 * Shift the trace to "just now" so it lands inside the default time windows,
 * preserving every relative offset and every duration exactly.
 */
const originalStart = Math.min(
  ...captured.spans.map((s) => new Date(s.span.start_time).getTime()),
);
const shiftMs = Date.now() - 60_000 - originalStart;

/** Group by (local project, service) — one Resource per OTLP group. */
const groups = new Map<string, { projectId: number; service: string; version: string | null; spans: SeedSpan[] }>();

for (const entry of captured.spans) {
  const localProjectId = projectMap.get(entry.project_id);
  if (!localProjectId) {
    throw new Error(
      `no local project mapped for original project ${entry.project_id} ` +
        `(${[...projectMap.keys()].join(", ")} are mapped)`,
    );
  }
  const s = entry.span;
  const startMs = new Date(s.start_time).getTime() + shiftMs;

  const key = `${localProjectId}:${s.resource.service_name}`;
  const group =
    groups.get(key) ??
    {
      projectId: localProjectId,
      service: s.resource.service_name,
      version: s.resource.service_version,
      spans: [] as SeedSpan[],
    };
  group.spans.push({
    traceId: s.trace_id,
    spanId: s.span_id,
    parentSpanId: s.parent_span_id ?? undefined,
    name: s.name,
    kind: KINDS[s.kind] ?? SpanKind.INTERNAL,
    // Durations carry sub-millisecond precision; start times do not (that is
    // the whole point of this fixture). Reconstruct end from start + duration
    // so the replay reproduces the original mismatch rather than smoothing it.
    startTimeUnixNano: BigInt(Math.round(startMs * 1e6)),
    endTimeUnixNano: BigInt(Math.round((startMs + s.duration_ms) * 1e6)),
    attributes: s.attributes as Record<string, AttrValue>,
    statusCode: STATUSES[s.status_code] ?? StatusCode.UNSET,
    statusMessage: s.status_message || undefined,
  });
  groups.set(key, group);
}

// One request per project (a batch may carry several Resources, but the
// project is fixed by the X-Temps-Project-Id header).
const byProject = new Map<number, ResourceSpanGroup[]>();
for (const group of groups.values()) {
  const list = byProject.get(group.projectId) ?? [];
  list.push({
    resourceAttributes: {
      "service.name": group.service,
      ...(group.version ? { "service.version": group.version } : {}),
    },
    scopeName: "temps-trace-replay",
    spans: group.spans,
  });
  byProject.set(group.projectId, list);
}

for (const [projectId, resourceGroups] of byProject) {
  const body = encodeExportTraceServiceRequest(resourceGroups);
  const res = await fetch(`${API}/api/otel/v1/traces`, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-protobuf",
      Authorization: `Bearer ${apiKey}`,
      "X-Temps-Project-Id": String(projectId),
    },
    body,
  });
  if (!res.ok) throw new Error(`ingest -> ${res.status}: ${(await res.text()).slice(0, 300)}`);
  if (!(res.headers.get("content-type") || "").includes("protobuf")) {
    throw new Error("ingest answered 200 but not from the OTLP handler (wrong path?)");
  }
  const count = resourceGroups.reduce((n, g) => n + g.spans.length, 0);
  console.log(`  ✓ project ${projectId}: ${count} spans`);
}

console.log(`\nReplayed trace ${captured.trace_id} into projects ${[...byProject.keys()].join(", ")}`);
