// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Background country backfill.
//
// When an event resolves a country, that instance's earlier NULL-country rows
// should inherit it (see backfillCountries in db/events.ts). Ingest must not do
// per-request database work beyond storing the event, so requests only enqueue
// (anonymous_id, country) into a bounded in-memory map, and a timer flushes the
// map with ONE set-based statement per interval.
//
// Bounds and failure policy:
// - At most `maxPending` distinct instances are queued (constant memory). Past
//   that, new instances are dropped and counted, and the count is logged on the
//   next flush. Nothing is lost for good: every later event from a dropped
//   instance enqueues it again.
// - A failed flush is logged and its batch discarded for the same reason, so
//   the queue's size never depends on database availability.
// - Flushes never overlap. `stop()` flushes whatever is pending on shutdown.
// - Within one interval the first country seen for an instance wins.

import type { Pool } from "pg";
import { backfillCountries } from "./db/events.js";
import { errorFields, log } from "./log.js";

// What the ingest routes depend on: a synchronous, non-blocking enqueue.
export interface CountryBackfillQueue {
  enqueue(anonymousIds: string[], country: string | null): void;
}

export interface CountryBackfillerOptions {
  maxPending: number;
  intervalMs: number;
}

export const DEFAULT_BACKFILL_OPTIONS: CountryBackfillerOptions = {
  maxPending: 10_000,
  intervalMs: 10_000,
};

export class CountryBackfiller implements CountryBackfillQueue {
  private pending = new Map<string, string>();
  private dropped = 0;
  private flushing: Promise<void> | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(
    private readonly pool: Pool,
    private readonly opts: CountryBackfillerOptions = DEFAULT_BACKFILL_OPTIONS
  ) {}

  get pendingCount(): number {
    return this.pending.size;
  }

  enqueue(anonymousIds: string[], country: string | null): void {
    if (!country) return;
    for (const id of anonymousIds) {
      if (this.pending.has(id)) continue;
      if (this.pending.size >= this.opts.maxPending) {
        this.dropped++;
        continue;
      }
      this.pending.set(id, country);
    }
  }

  // Flush everything queued so far. Never throws. If a flush is already in
  // flight, returns it; whatever was queued meanwhile goes out on the next one.
  flush(): Promise<void> {
    if (!this.flushing) {
      this.flushing = this.flushOnce().finally(() => {
        this.flushing = null;
      });
    }
    return this.flushing;
  }

  private async flushOnce(): Promise<void> {
    if (this.dropped > 0) {
      log("warn", "backfill", "queue full; instances dropped until their next event", {
        dropped: this.dropped,
        max_pending: this.opts.maxPending,
      });
      this.dropped = 0;
    }
    if (this.pending.size === 0) return;

    const batch = [...this.pending];
    this.pending = new Map();
    try {
      await backfillCountries(this.pool, batch);
    } catch (err) {
      log("error", "backfill", "country backfill flush failed; retried on each instance's next event", {
        instances: batch.length,
        ...errorFields(err),
      });
    }
  }

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => void this.flush(), this.opts.intervalMs);
  }

  // Stop the timer, wait for any in-flight flush, then flush what's left.
  async stop(): Promise<void> {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
    if (this.flushing) await this.flushing;
    await this.flush();
  }
}
