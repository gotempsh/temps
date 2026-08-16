/**
 * Seed a local Temps instance with a realistic OTel trace corpus.
 *
 * Creates several projects, each running several services, and pushes traces
 * through the real OTLP ingest endpoint (`POST /otel/v1/traces`) so the whole
 * pipeline — decode, quota, storage, trace summaries — is exercised exactly as
 * it is in production.
 *
 * The operation catalogue below is deliberately shaped so the span-stats API
 * has something interesting to rank:
 *
 *   - `cache.get` is trivially fast but runs on nearly every span tree, so it
 *     dominates *call count* while barely registering on p95.
 *   - `report.render` is uniformly slow, so it dominates *total time*.
 *   - `payments.charge` and `search.query` are bimodal (a fast path plus a
 *     long tail), so they dominate *inconsistency* — exactly the
 *     "takes anywhere from 15ms to 12s" case this data is meant to surface.
 *   - `inventory.reserve` fails ~12% of the time.
 *
 * Usage:
 *   bun run seed-otel-traces.ts
 *
 * Env:
 *   TEMPS_API           API base URL       (default http://localhost:8080)
 *   TEMPS_EMAIL         admin email        (default dev@temps.sh)
 *   TEMPS_PASSWORD      admin password     (required)
 *   TRACES_PER_PROJECT  traces per project (default 2500)
 *   HOURS_BACK          spread traces over the last N hours (default 24)
 */

import {
  encodeExportTraceServiceRequest,
  newSpanId,
  newTraceId,
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
const TRACES_PER_PROJECT = Number(process.env.TRACES_PER_PROJECT || 2500);
const HOURS_BACK = Number(process.env.HOURS_BACK || 24);
/** Spans per OTLP request. Ingest is rate-limited per project by request count. */
const SPANS_PER_BATCH = 2000;

if (!PASSWORD) {
  console.error("TEMPS_PASSWORD is required");
  process.exit(1);
}

// ── latency distributions ───────────────────────────────────────────

function gaussian(): number {
  // Box-Muller. `1 - random()` keeps the log argument off zero.
  const u = 1 - Math.random();
  const v = Math.random();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
}

/** Normally-distributed latency, clamped to a sane floor. */
function normal(meanMs: number, stddevMs: number): number {
  return Math.max(0.05, meanMs + gaussian() * stddevMs);
}

/**
 * Bimodal latency: a fast path taken `fastShare` of the time, and a slow path
 * otherwise. This is what an unindexed query, a cold cache, or a retry storm
 * actually looks like — and what an average alone completely hides.
 */
function bimodal(
  fastMean: number,
  fastSd: number,
  slowMean: number,
  slowSd: number,
  fastShare: number,
): number {
  return Math.random() < fastShare ? normal(fastMean, fastSd) : normal(slowMean, slowSd);
}

type LatencyFn = () => number;

interface Operation {
  name: string;
  kind: number;
  latencyMs: LatencyFn;
  /** Probability this span reports ERROR status. */
  errorRate: number;
  /** Probability this operation appears in a given trace. */
  frequency: number;
  attributes?: Record<string, AttrValue>;
}

interface ServiceDef {
  name: string;
  version: string;
  /** The entrypoint span for traces rooted at this service. */
  entrypoints: Operation[];
  /** Operations that hang off an entrypoint as children. */
  children: Operation[];
}

interface ProjectDef {
  name: string;
  slug: string;
  environment: string;
  services: ServiceDef[];
}

const CACHE_GET: Operation = {
  name: "cache.get",
  kind: SpanKind.CLIENT,
  // Very fast, very frequent — the "death by a thousand cuts" candidate.
  latencyMs: () => normal(0.9, 0.3),
  errorRate: 0,
  frequency: 1,
  attributes: { "db.system": "redis", "db.operation": "GET" },
};

const PROJECTS: ProjectDef[] = [
  {
    name: "Checkout Platform",
    slug: "checkout-platform",
    environment: "production",
    services: [
      {
        name: "checkout-api",
        version: "2.14.0",
        entrypoints: [
          {
            name: "POST /api/checkout",
            kind: SpanKind.SERVER,
            latencyMs: () => normal(340, 90),
            errorRate: 0.02,
            frequency: 1,
            attributes: { "http.request.method": "POST", "http.route": "/api/checkout" },
          },
          {
            name: "GET /api/cart",
            kind: SpanKind.SERVER,
            latencyMs: () => normal(48, 14),
            errorRate: 0.005,
            frequency: 1,
            attributes: { "http.request.method": "GET", "http.route": "/api/cart" },
          },
        ],
        children: [
          CACHE_GET,
          {
            name: "SELECT carts",
            kind: SpanKind.CLIENT,
            latencyMs: () => normal(18, 6),
            errorRate: 0.001,
            frequency: 0.9,
            attributes: { "db.system": "postgresql", "db.operation": "SELECT" },
          },
          {
            name: "UPDATE carts",
            kind: SpanKind.CLIENT,
            latencyMs: () => normal(26, 9),
            errorRate: 0.004,
            frequency: 0.45,
            attributes: { "db.system": "postgresql", "db.operation": "UPDATE" },
          },
        ],
      },
      {
        name: "payments-worker",
        version: "1.8.2",
        entrypoints: [
          {
            name: "payments.process",
            kind: SpanKind.CONSUMER,
            latencyMs: () => normal(520, 140),
            errorRate: 0.03,
            frequency: 1,
            attributes: { "messaging.system": "rabbitmq" },
          },
        ],
        children: [
          {
            name: "payments.charge",
            kind: SpanKind.CLIENT,
            // The headline inconsistency: usually ~120ms, but a 12% slice of
            // calls hits the provider's slow path and runs into seconds.
            latencyMs: () => bimodal(120, 25, 8600, 2400, 0.88),
            errorRate: 0.04,
            frequency: 1,
            attributes: { "peer.service": "stripe", "http.request.method": "POST" },
          },
          {
            name: "ledger.append",
            kind: SpanKind.CLIENT,
            latencyMs: () => normal(34, 8),
            errorRate: 0.002,
            frequency: 0.95,
            attributes: { "db.system": "postgresql", "db.operation": "INSERT" },
          },
          CACHE_GET,
        ],
      },
      {
        name: "inventory-service",
        version: "3.1.1",
        entrypoints: [
          {
            name: "GET /inventory/{sku}",
            kind: SpanKind.SERVER,
            latencyMs: () => normal(62, 20),
            errorRate: 0.01,
            frequency: 1,
            attributes: { "http.request.method": "GET", "http.route": "/inventory/{sku}" },
          },
        ],
        children: [
          {
            name: "inventory.reserve",
            kind: SpanKind.INTERNAL,
            latencyMs: () => normal(41, 12),
            // Genuinely flaky — optimistic locking losing races under load.
            errorRate: 0.12,
            frequency: 0.8,
          },
          CACHE_GET,
        ],
      },
    ],
  },
  {
    name: "Data Pipeline",
    slug: "data-pipeline",
    environment: "production",
    services: [
      {
        name: "ingest-worker",
        version: "0.9.4",
        entrypoints: [
          {
            name: "batch.ingest",
            kind: SpanKind.CONSUMER,
            latencyMs: () => normal(1250, 300),
            errorRate: 0.02,
            frequency: 1,
            attributes: { "messaging.system": "kafka" },
          },
        ],
        children: [
          {
            name: "s3.get_object",
            kind: SpanKind.CLIENT,
            latencyMs: () => normal(210, 70),
            errorRate: 0.008,
            frequency: 1,
            attributes: { "rpc.service": "S3", "rpc.method": "GetObject" },
          },
          {
            name: "parquet.decode",
            kind: SpanKind.INTERNAL,
            latencyMs: () => normal(430, 110),
            errorRate: 0.003,
            frequency: 1,
          },
          {
            name: "COPY events",
            kind: SpanKind.CLIENT,
            latencyMs: () => normal(380, 120),
            errorRate: 0.005,
            frequency: 0.9,
            attributes: { "db.system": "clickhouse", "db.operation": "INSERT" },
          },
        ],
      },
      {
        name: "reporting-service",
        version: "1.2.0",
        entrypoints: [
          {
            name: "GET /reports/{id}",
            kind: SpanKind.SERVER,
            latencyMs: () => normal(2100, 260),
            errorRate: 0.01,
            frequency: 1,
            attributes: { "http.request.method": "GET", "http.route": "/reports/{id}" },
          },
        ],
        children: [
          {
            name: "report.render",
            kind: SpanKind.INTERNAL,
            // Uniformly slow and always present — the total-time hog.
            latencyMs: () => normal(1750, 210),
            errorRate: 0.004,
            frequency: 1,
          },
          {
            name: "SELECT aggregates",
            kind: SpanKind.CLIENT,
            latencyMs: () => normal(290, 80),
            errorRate: 0.002,
            frequency: 1,
            attributes: { "db.system": "postgresql", "db.operation": "SELECT" },
          },
          CACHE_GET,
        ],
      },
    ],
  },
  {
    name: "Growth API",
    slug: "growth-api",
    environment: "staging",
    services: [
      {
        name: "growth-api",
        version: "4.0.0-rc3",
        entrypoints: [
          {
            name: "GET /api/search",
            kind: SpanKind.SERVER,
            latencyMs: () => normal(180, 60),
            errorRate: 0.015,
            frequency: 1,
            attributes: { "http.request.method": "GET", "http.route": "/api/search" },
          },
          {
            name: "POST /api/events",
            kind: SpanKind.SERVER,
            latencyMs: () => normal(22, 7),
            errorRate: 0.002,
            frequency: 1,
            attributes: { "http.request.method": "POST", "http.route": "/api/events" },
          },
        ],
        children: [
          {
            name: "search.query",
            kind: SpanKind.CLIENT,
            // Second inconsistency case, with a wider fast band: a warm index
            // answers in ~40ms, a cold shard takes 3-4 seconds.
            latencyMs: () => bimodal(42, 15, 3400, 900, 0.82),
            errorRate: 0.02,
            frequency: 0.85,
            attributes: { "db.system": "elasticsearch" },
          },
          {
            name: "feature_flags.evaluate",
            kind: SpanKind.INTERNAL,
            latencyMs: () => normal(3.2, 1.1),
            errorRate: 0,
            frequency: 1,
          },
          CACHE_GET,
        ],
      },
      {
        name: "notifications-worker",
        version: "1.5.7",
        entrypoints: [
          {
            name: "notification.dispatch",
            kind: SpanKind.CONSUMER,
            latencyMs: () => normal(310, 95),
            errorRate: 0.05,
            frequency: 1,
            attributes: { "messaging.system": "sqs" },
          },
        ],
        children: [
          {
            name: "smtp.send",
            kind: SpanKind.CLIENT,
            latencyMs: () => bimodal(240, 60, 5200, 1500, 0.93),
            errorRate: 0.06,
            frequency: 0.7,
            attributes: { "peer.service": "postmark" },
          },
          {
            name: "push.send",
            kind: SpanKind.CLIENT,
            latencyMs: () => normal(140, 45),
            errorRate: 0.03,
            frequency: 0.5,
            attributes: { "peer.service": "fcm" },
          },
          CACHE_GET,
        ],
      },
    ],
  },
];

// ── API helpers ─────────────────────────────────────────────────────

/** `/auth/login` authenticates with an HttpOnly `session` cookie, not a bearer token. */
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
    throw new Error(`${init.method || "GET"} ${path} -> ${res.status}: ${text.slice(0, 500)}`);
  }
  return (text ? JSON.parse(text) : undefined) as T;
}

async function login(): Promise<void> {
  const res = await fetch(`${API}/api/auth/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ email: EMAIL, password: PASSWORD }),
  });
  const body = await res.text();
  if (!res.ok) throw new Error(`login -> ${res.status}: ${body.slice(0, 300)}`);

  const setCookie = res.headers.getSetCookie?.() ?? [];
  const session = setCookie.map((c) => c.split(";")[0]!).find((c) => c.startsWith("session="));
  if (!session || session === "session=") {
    throw new Error(`login returned no session cookie: ${body.slice(0, 300)}`);
  }
  sessionCookie = session;
  console.log(`✓ logged in as ${EMAIL}`);
}

interface ProjectRow {
  id: number;
  name: string;
  slug?: string;
}

async function ensureProject(def: ProjectDef): Promise<number> {
  const existing = await api<{ projects?: ProjectRow[] }>("/projects?page_size=100");
  const match = (existing.projects ?? []).find((p) => p.name === def.name);
  if (match) {
    console.log(`  · project "${def.name}" already exists (id ${match.id})`);
    return match.id;
  }
  const created = await api<ProjectRow>("/projects", {
    method: "POST",
    body: JSON.stringify({
      name: def.name,
      directory: "/",
      main_branch: "main",
      preset: "dockerfile",
      project_type: "application",
      storage_service_ids: [],
      source_type: "docker_image",
    }),
  });
  console.log(`  ✓ created project "${def.name}" (id ${created.id})`);
  return created.id;
}

/** Mint the OTLP ingest key. See `admin-key.ts` for why this is not a direct insert. */
async function createIngestKey(): Promise<string> {
  const key = await mintApiKey(api, `otel-trace-seed-${Date.now()}`);
  console.log("✓ created OTLP ingest API key (MFA enrolled, used, and removed)");
  return key;
}

// ── trace generation ────────────────────────────────────────────────

/**
 * Convert fractional epoch-milliseconds to whole nanoseconds. Rounding to whole
 * milliseconds first would flatten the sub-millisecond operations (`cache.get`
 * averages 0.9ms) into a single bucket and destroy their variance.
 */
function nanos(epochMs: number): bigint {
  return BigInt(Math.round(epochMs * 1e6));
}

function makeSpan(
  op: Operation,
  traceId: string,
  spanId: string,
  parentSpanId: string | undefined,
  startMs: number,
  durationMs: number,
  isError: boolean,
): SeedSpan {
  return {
    traceId,
    spanId,
    parentSpanId,
    name: op.name,
    kind: op.kind,
    startTimeUnixNano: nanos(startMs),
    endTimeUnixNano: nanos(startMs + durationMs),
    attributes: { ...(op.attributes || {}) },
    statusCode: isError ? StatusCode.ERROR : StatusCode.OK,
    statusMessage: isError ? `${op.name} failed` : undefined,
  };
}

/**
 * Build one trace rooted at `service`. Children are laid out sequentially
 * inside the root's window so the waterfall reads naturally; the root's own
 * duration is stretched if the children overrun it.
 */
function buildTrace(service: ServiceDef, startMs: number): SeedSpan[] {
  const entrypoint = service.entrypoints[Math.floor(Math.random() * service.entrypoints.length)]!;
  const traceId = newTraceId();
  const rootSpanId = newSpanId();

  const children: SeedSpan[] = [];
  let cursor = startMs + 1;
  for (const op of service.children) {
    if (Math.random() > op.frequency) continue;
    const duration = op.latencyMs();
    const isError = Math.random() < op.errorRate;
    children.push(makeSpan(op, traceId, newSpanId(), rootSpanId, cursor, duration, isError));
    cursor += duration + 0.4;
  }

  const rootDuration = Math.max(entrypoint.latencyMs(), cursor - startMs + 1);
  const rootError =
    Math.random() < entrypoint.errorRate ||
    children.some((c) => c.statusCode === StatusCode.ERROR && Math.random() < 0.5);
  const root = makeSpan(
    entrypoint,
    traceId,
    rootSpanId,
    undefined,
    startMs,
    rootDuration,
    rootError,
  );

  return [root, ...children];
}

async function pushBatch(
  apiKey: string,
  projectId: number,
  groups: ResourceSpanGroup[],
): Promise<void> {
  const body = encodeExportTraceServiceRequest(groups);
  // Plugin routes — including OTLP ingest — are mounted under `/api`.
  const res = await fetch(`${API}/api/otel/v1/traces`, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-protobuf",
      Authorization: `Bearer ${apiKey}`,
      "X-Temps-Project-Id": String(projectId),
    },
    body,
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`OTLP ingest -> ${res.status}: ${text.slice(0, 500)}`);
  }
  // A wrong path falls through to the SPA, which happily answers 200 with
  // index.html — so a 200 alone proves nothing. Insist on the OTLP reply.
  const contentType = res.headers.get("content-type") || "";
  if (!contentType.includes("protobuf")) {
    throw new Error(
      `OTLP ingest answered 200 with content-type '${contentType}' — the request did not ` +
        `reach the ingest handler (wrong path or port?)`,
    );
  }
}

async function seedProject(def: ProjectDef, projectId: number, apiKey: string): Promise<number> {
  const windowMs = HOURS_BACK * 60 * 60 * 1000;
  const now = Date.now();

  // Group spans by service so each OTLP batch carries the right Resource.
  const pending = new Map<string, SeedSpan[]>();
  let pendingCount = 0;
  let totalSpans = 0;

  const flush = async () => {
    if (pendingCount === 0) return;
    const groups: ResourceSpanGroup[] = [];
    for (const [serviceName, spans] of pending) {
      if (spans.length === 0) continue;
      const service = def.services.find((s) => s.name === serviceName)!;
      groups.push({
        resourceAttributes: {
          "service.name": service.name,
          "service.version": service.version,
          "deployment.environment": def.environment,
        },
        scopeName: "temps-seed",
        spans,
      });
    }
    await pushBatch(apiKey, projectId, groups);
    totalSpans += pendingCount;
    pending.clear();
    pendingCount = 0;
  };

  for (let i = 0; i < TRACES_PER_PROJECT; i++) {
    const service = def.services[Math.floor(Math.random() * def.services.length)]!;
    // Bias trace starts toward the recent past so the default 1h/24h views
    // are populated rather than showing an empty tail.
    const age = Math.pow(Math.random(), 1.6) * windowMs;
    const spans = buildTrace(service, now - age);

    const bucket = pending.get(service.name) ?? [];
    bucket.push(...spans);
    pending.set(service.name, bucket);
    pendingCount += spans.length;

    if (pendingCount >= SPANS_PER_BATCH) await flush();
  }
  await flush();

  return totalSpans;
}

// ── main ────────────────────────────────────────────────────────────

async function main() {
  console.log(`Seeding OTel traces into ${API}`);
  await login();

  const apiKey = await createIngestKey();

  let grandTotalSpans = 0;
  const projectIds: number[] = [];
  for (const def of PROJECTS) {
    console.log(`\n${def.name}:`);
    const projectId = await ensureProject(def);
    projectIds.push(projectId);
    const started = Date.now();
    const spans = await seedProject(def, projectId, apiKey);
    grandTotalSpans += spans;
    console.log(
      `  ✓ ${TRACES_PER_PROJECT} traces / ${spans} spans across ` +
        `${def.services.length} services in ${((Date.now() - started) / 1000).toFixed(1)}s`,
    );
  }

  console.log("\nVerifying via the query API:");
  await verify(projectIds);

  console.log(
    `\nDone — ${PROJECTS.length * TRACES_PER_PROJECT} traces, ` +
      `${grandTotalSpans} spans over the last ${HOURS_BACK}h ` +
      `in projects ${projectIds.join(", ")}.`,
  );
}

/**
 * Read the data back through the query API. Ingest is batched and fails soft,
 * so "the POST returned 200" is not the same as "the spans are queryable".
 */
async function verify(projectIds: number[]): Promise<void> {
  const since = new Date(Date.now() - HOURS_BACK * 3600 * 1000).toISOString();
  for (const projectId of projectIds) {
    const res = await api<{ total: number }>(
      `/otel/trace-summaries?project_id=${projectId}&start_time=${since}&limit=1`,
    );
    console.log(`  project ${projectId}: ${res.total} traces queryable`);
    if (res.total === 0) {
      throw new Error(
        `project ${projectId} reports 0 traces after seeding — check the server log for ` +
          `ingest errors (quota, rate limit, or a decode failure)`,
      );
    }
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
