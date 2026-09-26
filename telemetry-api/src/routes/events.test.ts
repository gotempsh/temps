// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, mock } from "bun:test";
import { createEventsRoutes, KNOWN_EVENT_TYPES } from "./events.js";
import type { Pool } from "pg";

describe("KNOWN_EVENT_TYPES", () => {
  it("includes the runtime events plus the CLI setup event", () => {
    // 38 runtime events plus one CLI-only event.
    expect(KNOWN_EVENT_TYPES.size).toBe(39);
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
