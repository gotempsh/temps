// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// In-memory sliding-window limiter keyed by client IP. Fine for a
// single-process service (this one is): no shared state is needed, and a
// restart merely resets everyone's window rather than under- or over-counting.
//
// The client IP comes from X-Forwarded-For, which the Temps proxy overwrites
// with the address it observed, so a caller cannot rotate it to escape the
// limit. Memory is bounded: an IP holds at most `limit` timestamps, a refused
// call never leaves an empty entry behind, and IPs whose window has fully
// expired are swept once the map grows large.

const WINDOW_MS = 60_000;
const DEFAULT_SWEEP_THRESHOLD = 10_000;

export interface RateLimiter {
  // Spends `cost` units of `ip`'s budget. Returns true (and spends nothing)
  // when that would exceed the limit for the current window.
  isLimited(ip: string, cost?: number): boolean;
  // How many IPs currently hold an entry (for tests and diagnostics).
  trackedIps(): number;
}

export interface RateLimiterOptions {
  now?: () => number;
  // Map size above which expired IPs are swept, at most once per window.
  sweepThreshold?: number;
}

export function createRateLimiter(
  limitPerMinute: number,
  options: RateLimiterOptions = {}
): RateLimiter {
  const now = options.now ?? Date.now;
  const sweepThreshold = options.sweepThreshold ?? DEFAULT_SWEEP_THRESHOLD;
  const requestLog = new Map<string, number[]>();
  let lastSweep = Number.NEGATIVE_INFINITY;

  // Drops every IP whose window has fully expired. A full scan, so it runs at
  // most once per window: while more than `sweepThreshold` IPs are live, a
  // sweep frees nothing, and repeating it on every request would make each
  // request O(tracked IPs).
  const sweep = (at: number, cutoff: number): void => {
    if (requestLog.size <= sweepThreshold || at - lastSweep < WINDOW_MS) return;
    lastSweep = at;
    for (const [key, ts] of requestLog) {
      if (ts.every((t) => t <= cutoff)) requestLog.delete(key);
    }
  };

  return {
    isLimited(ip: string, cost = 1): boolean {
      const at = now();
      const cutoff = at - WINDOW_MS;
      const timestamps = (requestLog.get(ip) ?? []).filter((t) => t > cutoff);

      if (timestamps.length + cost > limitPerMinute) {
        // Keep only the live window. An IP refused with nothing spent (a cost
        // above the whole limit) holds no entry, so refusals alone can't grow
        // the map.
        if (timestamps.length > 0) requestLog.set(ip, timestamps);
        else requestLog.delete(ip);
        return true;
      }

      for (let i = 0; i < cost; i++) timestamps.push(at);
      requestLog.set(ip, timestamps);
      sweep(at, cutoff);
      return false;
    },

    trackedIps(): number {
      return requestLog.size;
    },
  };
}

export function rateLimitedResponse(): Response {
  return Response.json(
    { error: "rate limit exceeded, try again shortly" },
    { status: 429, headers: { "Retry-After": "60" } }
  );
}
