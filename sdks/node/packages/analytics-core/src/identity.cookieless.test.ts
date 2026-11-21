// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Cookieless mode is one-way for the page's lifetime, so these tests live in
// their own file: vitest gives every file a fresh module registry.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAnalytics } from "./Analytics";
import { getOrCreateSessionId, getOrCreateVisitorId, isCookielessIdentity } from "./identity";
import type { AnalyticsApi } from "./types";

describe("cookieless identity (ADR 109 D3)", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  let analytics: AnalyticsApi | null = null;

  beforeEach(() => {
    localStorage.clear();
    fetchMock = vi.fn().mockResolvedValue({ status: 204, ok: true });
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    analytics?.destroy();
    analytics = null;
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("deletes stored ids, writes nothing and sends no id", async () => {
    // What an earlier, persistent visit left behind.
    localStorage.setItem("temps_visitor_id", "visitor-from-yesterday");
    localStorage.setItem("temps_session_id", "session-from-yesterday");
    localStorage.setItem("temps_session_last_seen", "1");
    const setItem = vi.spyOn(Storage.prototype, "setItem");

    analytics = createAnalytics({
      cookieless: true,
      ignoreLocalhost: false,
      autoTrackPageviews: false,
      autoTrackPageLeave: false,
      autoTrackEngagement: false,
      autoTrackSpeedAnalytics: false,
    });
    await analytics.trackEvent("signup", { plan: "pro" });

    expect(isCookielessIdentity()).toBe(true);
    expect(localStorage.getItem("temps_visitor_id")).toBeNull();
    expect(localStorage.getItem("temps_session_id")).toBeNull();
    expect(localStorage.getItem("temps_session_last_seen")).toBeNull();
    expect(setItem).not.toHaveBeenCalled();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const body = JSON.parse(fetchMock.mock.calls[0][1].body as string) as Record<string, unknown>;
    expect(body.event_name).toBe("signup");
    expect(body).not.toHaveProperty("visitorId");
    expect(body).not.toHaveProperty("sessionId");
  });

  it("stays on for later calls, including an instance created without it", () => {
    analytics = createAnalytics({ cookieless: true, ignoreLocalhost: false, autoTrackPageviews: false });
    analytics.destroy();
    analytics = createAnalytics({ ignoreLocalhost: false, autoTrackPageviews: false });

    expect(getOrCreateVisitorId()).toBeUndefined();
    expect(getOrCreateSessionId()).toBeUndefined();
    expect(localStorage.length).toBe(0);
  });

  it("does not start session recording, even when asked to", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    analytics = createAnalytics({
      cookieless: true,
      ignoreLocalhost: false,
      autoTrackPageviews: false,
      autoTrackEngagement: false,
      autoTrackSpeedAnalytics: false,
      enableSessionRecording: true,
    });
    analytics.enableSessionRecording();

    expect(warn).toHaveBeenCalledWith(expect.stringContaining("cookieless"));
    // Starting a recorder posts a replay init request; none was sent.
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("keeps session recording off for a later instance created without the option", () => {
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    analytics = createAnalytics({ cookieless: true, ignoreLocalhost: false, autoTrackPageviews: false });
    analytics.destroy();
    analytics = createAnalytics({
      ignoreLocalhost: false,
      autoTrackPageviews: false,
      autoTrackEngagement: false,
      autoTrackSpeedAnalytics: false,
      enableSessionRecording: true,
    });
    analytics.enableSessionRecording();

    const replayCalls = fetchMock.mock.calls.filter(([url]) => String(url).includes("session-replay"));
    expect(replayCalls).toHaveLength(0);
    expect(localStorage.length).toBe(0);
  });
});
