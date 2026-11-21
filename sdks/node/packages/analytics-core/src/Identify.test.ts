// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAnalytics } from "./Analytics";
import type { AnalyticsApi } from "./types";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

const quiet = {
  ignoreLocalhost: false,
  autoTrackPageviews: false,
  autoTrackPageLeave: false,
  autoTrackEngagement: false,
  autoTrackSpeedAnalytics: false,
} as const;

describe("identify and reset (ADR 109 D4)", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  let analytics: AnalyticsApi | null = null;

  beforeEach(() => {
    localStorage.clear();
    fetchMock = vi.fn().mockImplementation(async (url: string) =>
      url.endsWith("/identify")
        ? jsonResponse(200, { applied: true, outcome: "updated", message: "ok" })
        : new Response(null, { status: 204 })
    );
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    analytics?.destroy();
    analytics = null;
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  function identifyCalls(): Array<[string, RequestInit]> {
    return fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/identify")) as Array<
      [string, RequestInit]
    >;
  }

  it("sends the user id, the traits and this browser's own visitor id", async () => {
    analytics = createAnalytics({ ...quiet, ingestKey: "tk_test" });
    await analytics.trackEvent("signup");
    const stored = analytics.getVisitorId();
    expect(stored).toBeTruthy();

    const result = await analytics.identify("user-42", { plan: "pro" });

    expect(result).toEqual({ applied: true, outcome: "updated", message: "ok" });
    const [[url, init]] = identifyCalls();
    expect(url).toBe("/api/_temps/identify");
    expect((init.headers as Record<string, string>)["X-Temps-Analytics-Key"]).toBe("tk_test");
    expect(JSON.parse(init.body as string)).toEqual({
      user_id: "user-42",
      traits: { plan: "pro" },
      visitor_id: stored,
    });
  });

  it("sends a repeated identify once, and again when the traits change", async () => {
    analytics = createAnalytics(quiet);
    await analytics.identify("user-42", { plan: "pro" });
    const repeat = await analytics.identify("user-42", { plan: "pro" });
    expect(repeat.outcome).toBe("already_sent");
    expect(identifyCalls()).toHaveLength(1);

    await analytics.identify("user-42", { plan: "team" });
    expect(identifyCalls()).toHaveLength(2);
  });

  it("does not remember an identify the server refused, so it can be retried", async () => {
    fetchMock.mockImplementation(async () =>
      jsonResponse(200, { applied: false, outcome: "visitor_not_found", message: "no visitor yet" })
    );
    analytics = createAnalytics(quiet);
    const first = await analytics.identify("user-42");
    expect(first).toMatchObject({ applied: false, outcome: "visitor_not_found" });
    await analytics.identify("user-42");
    expect(identifyCalls()).toHaveLength(2);
  });

  it("waits for the page view in flight before identifying", async () => {
    const order: string[] = [];
    let finishPageview: () => void = () => undefined;
    fetchMock.mockImplementation((url: string) => {
      order.push(String(url).endsWith("/identify") ? "identify" : "event");
      if (String(url).endsWith("/event")) {
        return new Promise<Response>((resolve) => {
          finishPageview = () => {
            order.push("event-done");
            resolve(new Response(null, { status: 204 }));
          };
        });
      }
      return Promise.resolve(jsonResponse(200, { applied: true, outcome: "updated", message: "ok" }));
    });
    analytics = createAnalytics(quiet);
    analytics.trackPageview();
    const pending = analytics.identify("user-42");
    await Promise.resolve();
    expect(order).toEqual(["event"]);
    finishPageview();
    await pending;
    expect(order).toEqual(["event", "event-done", "identify"]);
  });

  it("sends nothing for an identify that was waiting when reset() ran", async () => {
    let finishPageview: () => void = () => undefined;
    fetchMock.mockImplementation((url: string) => {
      if (String(url).endsWith("/event")) {
        return new Promise<Response>((resolve) => {
          finishPageview = () => resolve(new Response(null, { status: 204 }));
        });
      }
      if (String(url).endsWith("/identify")) {
        return Promise.resolve(jsonResponse(200, { applied: true, outcome: "updated", message: "ok" }));
      }
      return Promise.resolve(new Response(null, { status: 204 }));
    });
    analytics = createAnalytics(quiet);
    analytics.trackPageview();
    const pending = analytics.identify("signed-out-user");
    await analytics.reset();
    finishPageview();

    const result = await pending;
    expect(result).toMatchObject({ applied: false, outcome: "reset" });
    expect(identifyCalls()).toHaveLength(0);
    expect(localStorage.getItem("temps_identified")).toBeNull();
  });

  it("does not restore the memo from an identify answered after reset()", async () => {
    let answerIdentify: () => void = () => undefined;
    fetchMock.mockImplementation((url: string) => {
      if (String(url).endsWith("/identify")) {
        return new Promise<Response>((resolve) => {
          answerIdentify = () => resolve(jsonResponse(200, { applied: true, outcome: "updated", message: "ok" }));
        });
      }
      return Promise.resolve(new Response(null, { status: 204 }));
    });
    analytics = createAnalytics(quiet);
    const pending = analytics.identify("signed-out-user");
    await vi.waitFor(() => expect(identifyCalls()).toHaveLength(1));
    await analytics.reset();
    answerIdentify();
    await pending;
    expect(localStorage.getItem("temps_identified")).toBeNull();
  });

  it("reset() rejects when the server did not confirm it, after clearing local ids", async () => {
    analytics = createAnalytics(quiet);
    await analytics.trackEvent("signup");
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).endsWith("/reset")) throw new TypeError("Failed to fetch");
      return new Response(null, { status: 204 });
    });
    await expect(analytics.reset()).rejects.toMatchObject({ name: "ResetFailedError" });
    expect(analytics.getVisitorId()).toBeUndefined();

    fetchMock.mockImplementation(async () => new Response(null, { status: 503 }));
    await expect(analytics.reset()).rejects.toThrow(/answered 503/);

    fetchMock.mockImplementation(async () => new Response(null, { status: 204 }));
    await expect(analytics.reset()).resolves.toBeUndefined();
  });

  it("refuses an empty user id without a request", async () => {
    analytics = createAnalytics(quiet);
    const result = await analytics.identify("  ");
    expect(result.outcome).toBe("invalid_user_id");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("reset forgets the stored ids and the memo and expires the server cookies", async () => {
    analytics = createAnalytics(quiet);
    await analytics.trackEvent("signup");
    await analytics.identify("user-42");
    const before = analytics.getVisitorId();

    await analytics.reset();

    expect(analytics.getVisitorId()).toBeUndefined();
    expect(localStorage.getItem("temps_identified")).toBeNull();
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/reset"))).toBe(true);

    await analytics.trackEvent("next");
    expect(analytics.getVisitorId()).not.toBe(before);
    await analytics.identify("user-42");
    expect(identifyCalls()).toHaveLength(2);
  });

  it("a disabled tracker identifies nothing", async () => {
    analytics = createAnalytics({ ...quiet, disabled: true });
    const result = await analytics.identify("user-42");
    expect(result).toMatchObject({ applied: false, outcome: "disabled" });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
