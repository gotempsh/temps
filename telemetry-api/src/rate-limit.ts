// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// In-memory sliding-window limiter keyed by client IP. Fine for a
// single-process service (this one is): no shared state is needed, and a
// restart merely resets everyone's window rather than under- or over-counting.
//
// The client IP comes from X-Forwarded-For, which the Temps proxy overwrites
// with the address it observed, so a caller cannot rotate it to escape the
// limit. Memory is bounded: an IP holds at most `limit` timestamps, and IPs
// whose window has fully expired are swept once the map grows large.

const WINDOW_MS = 60_000;
const SWEEP_THRESHOLD = 10_000;

export interface RateLimiter {
  // Spends `cost` units of `ip`'s budget. Returns true (and spends nothing)
  // when that would exceed the limit for the current window.
  isLimited(ip: string, cost?: number): boolean;
}

export function createRateLimiter(
  limitPerMinute: number,
  now: () => number = Date.now
): RateLimiter {
  const requestLog = new Map<string, number[]>();

  return {
    isLimited(ip: string, cost = 1): boolean {
      const at = now();
      const cutoff = at - WINDOW_MS;
      const timestamps = (requestLog.get(ip) ?? []).filter((t) => t > cutoff);

      if (timestamps.length + cost > limitPerMinute) {
        requestLog.set(ip, timestamps);
        return true;
      }

      for (let i = 0; i < cost; i++) timestamps.push(at);
      requestLog.set(ip, timestamps);

      // Occasional sweep so long-running processes don't accumulate an
      // ever-growing map of one-off callers.
      if (requestLog.size > SWEEP_THRESHOLD) {
        for (const [key, ts] of requestLog) {
          if (ts.every((t) => t <= cutoff)) requestLog.delete(key);
        }
      }

      return false;
    },
  };
}

export function rateLimitedResponse(): Response {
  return Response.json(
    { error: "rate limit exceeded, try again shortly" },
    { status: 429, headers: { "Retry-After": "60" } }
  );
}
