// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Its own file: the switch to cookieless is one-way for the module registry.

import { afterEach, describe, expect, it, vi } from "vitest";
import { createAnalytics } from "./Analytics";
import { enableCookielessIdentity } from "./identity";

describe("identify when the page turns cookieless while it waits (ADR 109 D4)", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("sends nothing and stores no memo", async () => {
    localStorage.clear();
    let finishPageview: () => void = () => undefined;
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (String(url).endsWith("/event")) {
        return new Promise<Response>((resolve) => {
          finishPageview = () => resolve(new Response(null, { status: 204 }));
        });
      }
      return Promise.resolve(
        new Response(JSON.stringify({ applied: true, outcome: "updated", message: "ok" }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        })
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    const analytics = createAnalytics({
      ignoreLocalhost: false,
      autoTrackPageviews: false,
      autoTrackPageLeave: false,
      autoTrackEngagement: false,
      autoTrackSpeedAnalytics: false,
    });
    analytics.trackPageview();
    const pending = analytics.identify("user-42", { plan: "pro" });

    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    enableCookielessIdentity();
    finishPageview();

    await expect(pending).resolves.toMatchObject({ applied: false, outcome: "cookieless_project" });
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/identify"))).toBe(false);
    expect(localStorage.getItem("temps_identified")).toBeNull();
    analytics.destroy();
  });
});
