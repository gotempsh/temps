import { describe, it, expect, mock } from "bun:test";
import { createEventsRoutes, KNOWN_EVENT_TYPES } from "./events.js";
import type { Pool } from "pg";

describe("KNOWN_EVENT_TYPES", () => {
  it("stays in lockstep with the Rust binary (34 events)", () => {
    // Mirror of temps-core TelemetryEventKind::all().len(). If this changes,
    // update both this set and the Rust enum together.
    expect(KNOWN_EVENT_TYPES.size).toBe(34);
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
