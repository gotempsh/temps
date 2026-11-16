// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect } from "bun:test";
import { createRateLimiter, rateLimitedResponse } from "./rate-limit.js";

function clock(start = 1_000_000) {
  let t = start;
  return { now: () => t, advance: (ms: number) => void (t += ms) };
}

describe("createRateLimiter", () => {
  it("allows up to the limit per IP, then rejects", () => {
    const c = clock();
    const limiter = createRateLimiter(3, c.now);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(true);
  });

  it("keeps a separate budget per IP", () => {
    const limiter = createRateLimiter(1, clock().now);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    expect(limiter.isLimited("198.51.100.1")).toBe(true);
    expect(limiter.isLimited("198.51.100.2")).toBe(false);
  });

  it("frees the budget once the minute has passed", () => {
    const c = clock();
    const limiter = createRateLimiter(1, c.now);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
    c.advance(59_999);
    expect(limiter.isLimited("198.51.100.1")).toBe(true);
    c.advance(2);
    expect(limiter.isLimited("198.51.100.1")).toBe(false);
  });

  it("charges a cost all at once and refuses one that would overrun", () => {
    const limiter = createRateLimiter(10, clock().now);
    expect(limiter.isLimited("198.51.100.1", 8)).toBe(false);
    // 8 + 3 > 10: refused, and nothing is spent.
    expect(limiter.isLimited("198.51.100.1", 3)).toBe(true);
    expect(limiter.isLimited("198.51.100.1", 2)).toBe(false);
    expect(limiter.isLimited("198.51.100.1", 1)).toBe(true);
  });

  it("answers 429 with a Retry-After", async () => {
    const res = rateLimitedResponse();
    expect(res.status).toBe(429);
    expect(res.headers.get("Retry-After")).toBe("60");
    expect((await res.json()).error).toContain("rate limit");
  });
});
