// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, mock, spyOn } from "bun:test";
import type { Pool } from "pg";
import { CountryBackfiller } from "./backfill.js";

type Call = [string, unknown[]];

function makePool(impl: () => unknown = () => ({ rows: [] })) {
  const query = mock(impl);
  return { pool: { query } as unknown as Pool, calls: () => query.mock.calls as unknown as Call[] };
}

const opts = { maxPending: 1000, intervalMs: 60_000 };

describe("CountryBackfiller", () => {
  it("flushes every queued instance, each with its own country, in one statement", async () => {
    const { pool, calls } = makePool();
    const b = new CountryBackfiller(pool, opts);
    b.enqueue(["inst_1", "inst_2"], "US");
    b.enqueue(["inst_3"], "DE");
    await b.flush();

    expect(calls().length).toBe(1);
    const [sql, values] = calls()[0]!;
    expect(sql).toContain("unnest($1::text[], $2::text[])");
    expect(sql).toContain("UPDATE telemetry_instance_days");
    expect(sql).toContain("UPDATE telemetry_events");
    // Only NULLs are filled — a known country is never overwritten.
    expect(sql.match(/country IS NULL/g)?.length).toBe(2);
    expect(sql).toContain("event_type <> 'cli_setup_step'");
    expect(values).toEqual([["inst_1", "inst_2", "inst_3"], ["US", "US", "DE"]]);
    expect(b.pendingCount).toBe(0);
  });

  it("ignores unknown countries and keeps the first country seen per instance", async () => {
    const { pool, calls } = makePool();
    const b = new CountryBackfiller(pool, opts);
    b.enqueue(["inst_1"], null);
    expect(b.pendingCount).toBe(0);
    b.enqueue(["inst_1"], "FR");
    b.enqueue(["inst_1"], "DE");
    await b.flush();
    expect(calls()[0]![1]).toEqual([["inst_1"], ["FR"]]);
  });

  it("does nothing when the queue is empty", async () => {
    const { pool, calls } = makePool();
    await new CountryBackfiller(pool, opts).flush();
    expect(calls().length).toBe(0);
  });

  it("bounds the queue, rejects the overflow and reports it on the next flush", async () => {
    const warn = spyOn(console, "error").mockImplementation(() => {});
    const { pool, calls } = makePool();
    const b = new CountryBackfiller(pool, { maxPending: 2, intervalMs: 60_000 });
    b.enqueue(["inst_1", "inst_2", "inst_3", "inst_4"], "US");
    expect(b.pendingCount).toBe(2);
    await b.flush();
    const line = JSON.parse(warn.mock.calls[0]![0] as string);
    warn.mockRestore();

    expect(line).toMatchObject({ level: "warn", component: "backfill", rejected_enqueues: 2, max_pending: 2 });
    expect(calls()[0]![1]).toEqual([["inst_1", "inst_2"], ["US", "US"]]);
  });

  it("never throws on a failed flush, logs it and clears the batch", async () => {
    const err = spyOn(console, "error").mockImplementation(() => {});
    const { pool } = makePool(() => { throw new Error("db down"); });
    const b = new CountryBackfiller(pool, opts);
    b.enqueue(["inst_1"], "US");
    await b.flush();
    const line = JSON.parse(err.mock.calls[0]![0] as string);
    err.mockRestore();

    expect(line).toMatchObject({ level: "error", component: "backfill", instances: 1, error: "db down" });
    expect(b.pendingCount).toBe(0);
  });

  it("never runs two flushes at once", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const { pool, calls } = makePool(() => gate.then(() => ({ rows: [] })));
    const b = new CountryBackfiller(pool, opts);
    b.enqueue(["inst_1"], "US");
    const first = b.flush();
    b.enqueue(["inst_2"], "US");
    const second = b.flush();
    expect(second).toBe(first);
    release();
    await first;
    expect(calls().length).toBe(1);
    expect(b.pendingCount).toBe(1); // inst_2 goes out on the next flush
  });

  it("flushes on its interval and drains what's pending on stop", async () => {
    const { pool, calls } = makePool();
    const b = new CountryBackfiller(pool, { maxPending: 1000, intervalMs: 20 });
    b.start();
    b.enqueue(["inst_1"], "US");
    await Bun.sleep(60);
    expect(calls().length).toBe(1);

    b.enqueue(["inst_2"], "DE");
    await b.stop();
    expect(calls().length).toBe(2);
    expect(calls()[1]![1]).toEqual([["inst_2"], ["DE"]]);

    await Bun.sleep(60); // timer is cleared: no further flushes
    expect(calls().length).toBe(2);
  });
});
