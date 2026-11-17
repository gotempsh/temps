// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect } from "bun:test";
import { createRateLimiter, rateLimitAddress, rateLimitedResponse } from "./rate-limit.js";

function clock(start = 1_000_000) {
  let t = start;
  return { now: () => t, advance: (ms: number) => void (t += ms) };
}

describe("createRateLimiter", () => {
  it("allows up to the limit per IP, then rejects", () => {
    const c = clock();
    const limiter = createRateLimiter(3, { now: c.now });
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(true);
  });

  it("keeps a separate budget per IP", () => {
    const limiter = createRateLimiter(1, { now: clock().now });
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(true);
    expect(limiter.isLimited("198.51.100.2")).toBe(false);
  });

  it("frees the budget once the minute has passed", () => {
    const c = clock();
    const limiter = createRateLimiter(1, { now: c.now });
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    c.advance(59_999);
    expect(limiter.isLimited("198.51.100.1")).toBe(true);
    c.advance(2);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
  });

  it("charges a cost all at once and refuses one that would overrun", () => {
    const limiter = createRateLimiter(10, { now: clock().now });
    expect(limiter.isLimited("198.51.100.1", 8)).toBe(false);
    // 8 + 3 > 10: refused, and nothing is spent.
    expect(limiter.isLimited("198.51.100.1", 3)).toBe(true);
    expect(limiter.isLimited("198.51.100.1", 2)).toBe(false);
    expect(limiter.isLimited("198.51.100.1", 1)).toBe(true);
  });

  it("leaves no entry behind for an IP refused with nothing spent", () => {
    const limiter = createRateLimiter(5, { now: clock().now });
    // A cost above the whole limit, from addresses never seen before.
    for (let i = 0; i < 50; i++) {
      expect(limiter.isLimited(`198.51.100.${i}`, 6)).toBe(true);
    }
    expect(limiter.trackedIps()).toBe(0);
  });

  it("keeps the live window of an IP it refuses", () => {
    const limiter = createRateLimiter(5, { now: clock().now });
    expect(limiter.isLimited("198.51.100.1", 4)).toBe(false);
    expect(limiter.isLimited("198.51.100.1", 6)).toBe(true);
    expect(limiter.trackedIps()).toBe(1);
    // The 4 units already spent still count.
    expect(limiter.isLimited("198.51.100.1", 2)).toBe(true);
    expect(limiter.isLimited("198.51.100.1", 1)).toBe(false);
  });

  it("sweeps expired IPs above the threshold, at most once per window", () => {
    const c = clock(0);
    const limiter = createRateLimiter(10, { now: c.now, sweepThreshold: 1 });
    limiter.isLimited("a"); // t=0
    c.advance(50_000);
    limiter.isLimited("b"); // t=50s: 2 > 1, sweeps (nothing expired yet)
    expect(limiter.trackedIps()).toBe(2);

    c.advance(20_000);
    limiter.isLimited("c"); // t=70s: "a" expired, but the last sweep was 20s ago
    expect(limiter.trackedIps()).toBe(3);

    c.advance(41_000);
    limiter.isLimited("d"); // t=111s: a window since the last sweep; "a" and "b" go
    expect(limiter.trackedIps()).toBe(2);
  });

  it("answers 429 with a Retry-After", async () => {
    const res = rateLimitedResponse();
    expect(res.status).toBe(429);
    expect(res.headers.get("Retry-After")).toBe("60");
    expect((await res.json()).error).toContain("rate limit");
  });
});

describe("rateLimitAddress", () => {
  const req = (headers: Record<string, string>) => new Request("http://localhost/", { headers });

  it("uses the address the edge proxy wrote: the rightmost forwarded entry", () => {
    expect(rateLimitAddress(req({ "x-forwarded-for": "198.51.100.7" }), "10.0.0.2")).toBe("198.51.100.7");
    // Earlier entries are whatever the caller sent.
    expect(rateLimitAddress(req({ "x-forwarded-for": "1.2.3.4, 5.6.7.8,198.51.100.7 " }), null)).toBe("198.51.100.7");
  });

  it("never trusts X-Real-IP, falling back to the TCP peer", () => {
    expect(rateLimitAddress(req({ "x-real-ip": "203.0.113.9" }), "10.0.0.2")).toBe("10.0.0.2");
    expect(rateLimitAddress(req({ "x-forwarded-for": " , " }), "10.0.0.2")).toBe("10.0.0.2");
    expect(rateLimitAddress(req({}), null)).toBeNull();
  });
});
