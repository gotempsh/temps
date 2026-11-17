// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, mock } from "bun:test";
import { createEventsRoutes, KNOWN_EVENT_TYPES, type EventLimits } from "./events.js";
import { createRateLimiter } from "../rate-limit.js";
import type { Pool } from "pg";

describe("KNOWN_EVENT_TYPES", () => {
  it("includes the runtime events plus the CLI setup event", () => {
    // 51 runtime events plus one CLI-only event.
    expect(KNOWN_EVENT_TYPES.size).toBe(52);
  });

  it("uses only snake_case names", () => {
    for (const name of KNOWN_EVENT_TYPES) {
      expect(name).toMatch(/^[a-z][a-z0-9_]*$/);
    }
  });
});

// Records what the routes enqueue for background backfill.
function makeQueue() {
  const queued: Array<[string[], string | null]> = [];
  return { queued, enqueue: (ids: string[], country: string | null) => void queued.push([ids, country]) };
}
const noBackfill = { enqueue: () => {} };

function makePool(queryFn: () => unknown = () => ({ rows: [] })) {
  return {
    query: mock(queryFn),
  } as unknown as Pool;
}

function makeReq(body: unknown, method = "POST"): Request {
  return new Request("http://localhost/v1/events", {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

describe("onboarding milestone ingest", () => {
  for (const eventType of [
    "first_deploy_attempted",
    "first_project_created",
    "first_git_provider_connected",
  ]) {
    for (const batch of [false, true]) {
      it(`stores ${eventType} through ${batch ? "batch" : "single"} ingest`, async () => {
        const pool = makePool();
        const routes = createEventsRoutes(pool, {
          backfill: noBackfill,
          resolveCountry: () => null,
        });
        const event = {
          anonymous_id: "inst_sample",
          event_type: eventType,
          properties: {},
        };
        const res = batch
          ? await routes.postBatch(makeReq({ events: [event] }))
          : await routes.postEvent(makeReq(event));

        expect(res.status).toBe(201);
        const calls = (pool.query as ReturnType<typeof mock>).mock.calls as unknown as
          Array<[string, unknown[]]>;
        expect(calls).toHaveLength(2);
        expect(calls[0][1][1]).toBe(eventType);
        expect(JSON.parse(calls[0][1][2] as string)).toEqual({});
      });
    }
  }
});

describe("POST /v1/events", () => {
  const setupEvent = {
    anonymous_id: "12345678-1234-4234-8234-123456789abc",
    event_type: "cli_setup_step",
    properties: { step: "install", status: "completed", method: "ssh", elapsed_bucket: "under_minute", cli_version: "0.1.36" },
  };

  it("accepts setup events without counting a CLI attempt as an active server", async () => {
    const pool = makePool();
    const res = await createEventsRoutes(pool, { backfill: noBackfill }).postEvent(makeReq(setupEvent));
    expect(res.status).toBe(201);
    expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(1);
    const values = (pool.query as ReturnType<typeof mock>).mock.calls[0]?.[1];
    expect(values[5]).toBeNull();
  });

  it("rejects setup fields that could contain identifiers or raw errors", async () => {
    for (const extra of [{ host: "private.example" }, { email: "admin@example.com" }, { error: "secret" }, { constructor: "unexpected" }]) {
      const pool = makePool();
      const res = await createEventsRoutes(pool, { backfill: noBackfill }).postEvent(makeReq({ ...setupEvent, properties: { ...setupEvent.properties, ...extra } }));
      expect(res.status).toBe(422);
      expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(0);
    }
  });

  it("rejects arbitrary setup statuses and non-random identifiers", async () => {
    const pool = makePool();
    expect((await createEventsRoutes(pool, { backfill: noBackfill }).postEvent(makeReq({ ...setupEvent, anonymous_id: "server.example" }))).status).toBe(422);
    expect((await createEventsRoutes(pool, { backfill: noBackfill }).postEvent(makeReq({ ...setupEvent, properties: { ...setupEvent.properties, status: "private error" } }))).status).toBe(422);
  });

  it("accepts a valid event", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool, { backfill: noBackfill });

    const res = await postEvent(
      makeReq({
        anonymous_id: "inst_abc123",
        event_type: "deploy_succeeded",
        temps_version: "0.1.0",
        properties: { service_name: "my-app", duration_ms: 4200 },
      })
    );

    expect(res.status).toBe(201);
    const json = await res.json();
    expect(json.ok).toBe(true);
    expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(2); // insert + upsert
  });

  it("rejects invalid JSON", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool, { backfill: noBackfill });

    const req = new Request("http://localhost/v1/events", {
      method: "POST",
      body: "not json",
    });
    const res = await postEvent(req);
    expect(res.status).toBe(400);
  });

  it("rejects unknown event_type", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool, { backfill: noBackfill });

    const res = await postEvent(
      makeReq({ anonymous_id: "inst_abc123", event_type: "random_garbage" })
    );
    expect(res.status).toBe(422);
    const json = await res.json();
    expect(json.error).toMatch(/unknown event_type/);
  });

  it("rejects missing anonymous_id", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool, { backfill: noBackfill });

    const res = await postEvent(
      makeReq({ event_type: "deploy_attempted" })
    );
    expect(res.status).toBe(422);
  });

  it("strips PII keys from properties", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool, { backfill: noBackfill });

    await postEvent(
      makeReq({
        anonymous_id: "inst_abc123",
        event_type: "deploy_attempted",
        properties: { email: "user@example.com", service_name: "app" },
      })
    );

    const insertCall = (pool.query as ReturnType<typeof mock>).mock.calls[0];
    const propertiesArg = insertCall[1][2] as string;
    const props = JSON.parse(propertiesArg);
    expect(props.email).toBeUndefined();
    expect(props.service_name).toBe("app");
  });
});

describe("POST /v1/events/batch", () => {
  it("accepts a valid batch", async () => {
    const pool = makePool();
    const { postBatch } = createEventsRoutes(pool, { backfill: noBackfill });

    const req = new Request("http://localhost/v1/events/batch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        events: [
          { anonymous_id: "inst_1", event_type: "deploy_attempted" },
          { anonymous_id: "inst_1", event_type: "deploy_succeeded" },
        ],
      }),
    });

    const res = await postBatch(req);
    expect(res.status).toBe(201);
    const json = await res.json();
    expect(json.accepted).toBe(2);
  });

  it("rejects batch over 100 events", async () => {
    const pool = makePool();
    const { postBatch } = createEventsRoutes(pool, { backfill: noBackfill });

    const events = Array.from({ length: 101 }, () => ({
      anonymous_id: "inst_1",
      event_type: "deploy_attempted",
    }));
    const req = new Request("http://localhost/v1/events/batch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ events }),
    });

    const res = await postBatch(req);
    expect(res.status).toBe(422);
  });

  it("rejects batch with any invalid event", async () => {
    const pool = makePool();
    const { postBatch } = createEventsRoutes(pool, { backfill: noBackfill });

    const req = new Request("http://localhost/v1/events/batch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        events: [
          { anonymous_id: "inst_1", event_type: "deploy_attempted" },
          { anonymous_id: "inst_1", event_type: "BOGUS_EVENT" },
        ],
      }),
    });

    const res = await postBatch(req);
    expect(res.status).toBe(422);
    const json = await res.json();
    expect(json.details[0].index).toBe(1);
  });
});

describe("country backfill (request path)", () => {
  type Call = [string, unknown[]];
  const calls = (pool: Pool) =>
    (pool.query as ReturnType<typeof mock>).mock.calls as unknown as Call[];
  // The request path may only store events (INSERT, incl. the instance-day
  // upsert's ON CONFLICT DO UPDATE); the backfill UPDATE runs in the background.
  const updates = (pool: Pool) =>
    calls(pool).filter(([sql]) => !sql.trimStart().startsWith("INSERT"));

  function batchReq(events: unknown[]): Request {
    return new Request("http://localhost/v1/events/batch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ events }),
    });
  }

  it("queues the instance with the resolved country and runs no UPDATE on the request path", async () => {
    const pool = makePool();
    const queue = makeQueue();
    const res = await createEventsRoutes(pool, { backfill: queue, resolveCountry: () => "DE" }).postEvent(
      makeReq({ anonymous_id: "inst_abc", event_type: "instance_started" })
    );
    expect(res.status).toBe(201);
    expect(queue.queued).toEqual([[["inst_abc"], "DE"]]);
    expect(updates(pool).length).toBe(0);
  });

  it("queues each distinct instance of a batch once, with no UPDATE", async () => {
    const pool = makePool();
    const queue = makeQueue();
    const res = await createEventsRoutes(pool, { backfill: queue, resolveCountry: () => "US" }).postBatch(
      batchReq([
        { anonymous_id: "inst_1", event_type: "deploy_attempted" },
        { anonymous_id: "inst_1", event_type: "deploy_succeeded" },
        { anonymous_id: "inst_2", event_type: "instance_started" },
      ])
    );
    expect(res.status).toBe(201);
    expect(queue.queued).toEqual([[["inst_1", "inst_2"], "US"]]);
    expect(updates(pool).length).toBe(0);
  });

  it("never queues CLI setup attempts", async () => {
    const queue = makeQueue();
    await createEventsRoutes(makePool(), { backfill: queue, resolveCountry: () => "DE" }).postEvent(makeReq({
      anonymous_id: "12345678-1234-4234-8234-123456789abc",
      event_type: "cli_setup_step",
      properties: { step: "install", status: "completed", method: "ssh", elapsed_bucket: "under_minute", cli_version: "0.1.36" },
    }));
    expect(queue.queued).toEqual([[[], "DE"]]);
  });

  it("does not queue anything when the event can't be stored", async () => {
    const queue = makeQueue();
    const pool = makePool(() => { throw new Error("insert boom"); });
    const res = await createEventsRoutes(pool, { backfill: queue, resolveCountry: () => "DE" }).postEvent(
      makeReq({ anonymous_id: "inst_abc", event_type: "instance_started" })
    );
    expect(res.status).toBe(500);
    expect(queue.queued.length).toBe(0);
  });
});

describe("ingest abuse limits", () => {
  const event = (properties: Record<string, unknown> = {}) => ({
    anonymous_id: "instance-abcdef12",
    event_type: "deploy_attempted",
    properties,
  });

  function reqFrom(ip: string, body: unknown, path = "/v1/events"): Request {
    return new Request(`http://localhost${path}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "x-forwarded-for": ip },
      body: JSON.stringify(body),
    });
  }

  function routes(pool: Pool, eventsPerMinute: number, overrides: Partial<EventLimits> = {}) {
    return createEventsRoutes(pool, {
      backfill: noBackfill,
      rateLimiter: createRateLimiter(eventsPerMinute),
      limits: {
        eventPropertiesMaxBytes: 8192,
        eventsBatchMaxEvents: 100,
        tempsVersionMaxChars: 64,
        ...overrides,
      },
    });
  }

  it("429s single events past the per-IP budget without touching the database", async () => {
    const pool = makePool();
    const { postEvent } = routes(pool, 2);
    expect((await postEvent(reqFrom("198.51.100.10", event()))).status).toBe(201);
    expect((await postEvent(reqFrom("198.51.100.10", event()))).status).toBe(201);
    const calls = (pool.query as ReturnType<typeof mock>).mock.calls.length;

    const limited = await postEvent(reqFrom("198.51.100.10", event()));
    expect(limited.status).toBe(429);
    expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(calls);
    // Another IP is unaffected.
    expect((await postEvent(reqFrom("198.51.100.11", event()))).status).toBe(201);
  });

  it("charges a batch one unit per event, shared with the single-event route", async () => {
    const pool = makePool();
    const { postEvent, postBatch } = routes(pool, 5);
    const ip = "198.51.100.12";
    const batch = (n: number) => reqFrom(ip, { events: Array.from({ length: n }, () => event()) }, "/v1/events/batch");

    expect((await postBatch(batch(4))).status).toBe(201);
    // 4 + 2 > 5: the whole batch is refused.
    expect((await postBatch(batch(2))).status).toBe(429);
    expect((await postEvent(reqFrom(ip, event()))).status).toBe(201);
    expect((await postEvent(reqFrom(ip, event()))).status).toBe(429);
  });

  it("charges malformed batch bodies one unit, until the IP is out of budget", async () => {
    const pool = makePool();
    const { postBatch } = routes(pool, 3);
    const ip = "198.51.100.15";
    const malformed = () =>
      new Request("http://localhost/v1/events/batch", {
        method: "POST",
        headers: { "x-forwarded-for": ip },
        body: "not json",
      });

    expect((await postBatch(malformed())).status).toBe(400);
    expect((await postBatch(malformed())).status).toBe(400);
    expect((await postBatch(malformed())).status).toBe(400);
    expect((await postBatch(malformed())).status).toBe(429);
    expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(0);
  });

  it("charges a valid batch its event count exactly once", async () => {
    const pool = makePool();
    const { postBatch } = routes(pool, 5);
    const ip = "198.51.100.16";
    const batch = (n: number) => reqFrom(ip, { events: Array.from({ length: n }, () => event()) }, "/v1/events/batch");

    // 2 events cost 2, then 3 more fit exactly: charged once each, not twice.
    expect((await postBatch(batch(2))).status).toBe(201);
    expect((await postBatch(batch(3))).status).toBe(201);
    expect((await postBatch(batch(1))).status).toBe(429);
  });

  it("spends the budget on rejected payloads too", async () => {
    const pool = makePool();
    const { postEvent } = routes(pool, 1);
    const ip = "198.51.100.13";
    const invalid = await postEvent(reqFrom(ip, { anonymous_id: "instance-abcdef12", event_type: "nope" }));
    expect(invalid.status).toBe(422);
    expect((await postEvent(reqFrom(ip, event()))).status).toBe(429);
  });

  it("fails open when no client IP is resolvable", async () => {
    const pool = makePool();
    const { postEvent } = routes(pool, 1);
    for (let i = 0; i < 3; i++) {
      expect((await postEvent(makeReq(event()))).status).toBe(201);
    }
  });

  it("rejects properties over the byte cap, in single events and batches", async () => {
    const pool = makePool();
    const { postEvent, postBatch } = routes(pool, 1000, { eventPropertiesMaxBytes: 100 });
    const big = event({ blob: "x".repeat(200) });

    const single = await postEvent(makeReq(big));
    expect(single.status).toBe(422);
    expect((await single.json()).error).toContain("properties exceeds 100 bytes");

    const batch = await postBatch(
      new Request("http://localhost/v1/events/batch", {
        method: "POST",
        body: JSON.stringify({ events: [event(), big] }),
      })
    );
    expect(batch.status).toBe(422);
    expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(0);

    expect((await postEvent(makeReq(event({ blob: "x".repeat(50) })))).status).toBe(201);
  });

  it("measures the cap in UTF-8 bytes, not characters", async () => {
    const pool = makePool();
    const { postEvent } = routes(pool, 1000, { eventPropertiesMaxBytes: 100 });
    // 40 chars of a 3-byte character: under 100 chars, over 100 bytes.
    const res = await postEvent(makeReq(event({ note: "€".repeat(40) })));
    expect(res.status).toBe(422);
  });

  it("rejects an oversized temps_version", async () => {
    const pool = makePool();
    const { postEvent } = routes(pool, 1000);
    const res = await postEvent(makeReq({ ...event(), temps_version: "1".repeat(65) }));
    expect(res.status).toBe(422);
    expect((await postEvent(makeReq({ ...event(), temps_version: "0.1.36" }))).status).toBe(201);
  });

  it("uses the configured temps_version cap", async () => {
    const pool = makePool();
    const { postEvent } = routes(pool, 1000, { tempsVersionMaxChars: 5 });
    const res = await postEvent(makeReq({ ...event(), temps_version: "0.1.36" }));
    expect(res.status).toBe(422);
    expect((await res.json()).error).toContain("temps_version exceeds 5 chars");
    expect((await postEvent(makeReq({ ...event(), temps_version: "0.1.9" }))).status).toBe(201);
  });

  it("uses the configured batch size cap", async () => {
    const pool = makePool();
    const { postBatch } = routes(pool, 1000, { eventsBatchMaxEvents: 3 });
    const batch = (n: number) =>
      new Request("http://localhost/v1/events/batch", {
        method: "POST",
        body: JSON.stringify({ events: Array.from({ length: n }, () => event()) }),
      });
    expect((await postBatch(batch(3))).status).toBe(201);
    const tooMany = await postBatch(batch(4));
    expect(tooMany.status).toBe(422);
    expect((await tooMany.json()).error).toBe("max 3 events per batch");
  });

  it("charges an oversized batch the batch cap, not its full length", async () => {
    const pool = makePool();
    const { postBatch } = routes(pool, 5, { eventsBatchMaxEvents: 3 });
    const ip = "198.51.100.14";
    const batch = (n: number) => reqFrom(ip, { events: Array.from({ length: n }, () => event()) }, "/v1/events/batch");
    // 50 events, rejected as too many, but charged 3, leaving 2.
    expect((await postBatch(batch(50))).status).toBe(422);
    expect((await postBatch(batch(2))).status).toBe(201);
    expect((await postBatch(batch(1))).status).toBe(429);
  });
});
