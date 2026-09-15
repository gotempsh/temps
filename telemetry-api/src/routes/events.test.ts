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
    const res = await createEventsRoutes(pool).postEvent(makeReq(setupEvent));
    expect(res.status).toBe(201);
    expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(1);
    const values = (pool.query as ReturnType<typeof mock>).mock.calls[0]?.[1];
    expect(values[5]).toBeNull();
  });

  it("rejects setup fields that could contain identifiers or raw errors", async () => {
    for (const extra of [{ host: "private.example" }, { email: "admin@example.com" }, { error: "secret" }, { constructor: "unexpected" }]) {
      const pool = makePool();
      const res = await createEventsRoutes(pool).postEvent(makeReq({ ...setupEvent, properties: { ...setupEvent.properties, ...extra } }));
      expect(res.status).toBe(422);
      expect((pool.query as ReturnType<typeof mock>).mock.calls.length).toBe(0);
    }
  });

  it("rejects arbitrary setup statuses and non-random identifiers", async () => {
    const pool = makePool();
    expect((await createEventsRoutes(pool).postEvent(makeReq({ ...setupEvent, anonymous_id: "server.example" }))).status).toBe(422);
    expect((await createEventsRoutes(pool).postEvent(makeReq({ ...setupEvent, properties: { ...setupEvent.properties, status: "private error" } }))).status).toBe(422);
  });

  it("accepts a valid event", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool);

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
    const { postEvent } = createEventsRoutes(pool);

    const req = new Request("http://localhost/v1/events", {
      method: "POST",
      body: "not json",
    });
    const res = await postEvent(req);
    expect(res.status).toBe(400);
  });

  it("rejects unknown event_type", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool);

    const res = await postEvent(
      makeReq({ anonymous_id: "inst_abc123", event_type: "random_garbage" })
    );
    expect(res.status).toBe(422);
    const json = await res.json();
    expect(json.error).toMatch(/unknown event_type/);
  });

  it("rejects missing anonymous_id", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool);

    const res = await postEvent(
      makeReq({ event_type: "deploy_attempted" })
    );
    expect(res.status).toBe(422);
  });

  it("strips PII keys from properties", async () => {
    const pool = makePool();
    const { postEvent } = createEventsRoutes(pool);

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
    const { postBatch } = createEventsRoutes(pool);

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
    const { postBatch } = createEventsRoutes(pool);

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
    const { postBatch } = createEventsRoutes(pool);

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
