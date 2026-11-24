// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider } from "../Provider";

describe("TempsAnalyticsProvider cookieless (ADR 109 D3)", () => {
  beforeEach(() => {
    window.location = {
      ...window.location,
      hostname: "example.com",
      host: "example.com",
      pathname: "/pricing",
      search: "",
      protocol: "https:",
      href: "https://example.com/pricing",
    };
    vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 204 }));
    localStorage.clear();
  });

  it("sends the first page view without an id and stores nothing", () => {
    localStorage.setItem("temps_visitor_id", "visitor-from-yesterday");

    render(
      <TempsAnalyticsProvider cookieless autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
        <p>page</p>
      </TempsAnalyticsProvider>
    );

    const fetchMock = global.fetch as unknown as ReturnType<typeof vi.fn>;
    const bodies = fetchMock.mock.calls.map((call) => JSON.parse(call[1].body as string) as Record<string, unknown>);
    expect(bodies.map((body) => body.event_name)).toEqual(["page_view"]);
    expect(bodies[0]).not.toHaveProperty("visitorId");
    expect(bodies[0]).not.toHaveProperty("sessionId");
    expect(localStorage.getItem("temps_visitor_id")).toBeNull();
    expect(localStorage.getItem("temps_session_id")).toBeNull();
  });
});
