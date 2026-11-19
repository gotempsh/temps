// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, renderHook } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider, useTempsAnalytics } from "../Provider";

/** Event names the provider sent through fetch or sendBeacon, in order. */
function sentEventNames(): string[] {
  const fetchMock = global.fetch as unknown as ReturnType<typeof vi.fn>;
  const beaconMock = navigator.sendBeacon as unknown as ReturnType<typeof vi.fn>;
  const fromFetch = fetchMock.mock.calls.map((call) => JSON.parse(call[1].body as string).event_name as string);
  const fromBeacon = beaconMock.mock.calls.map((call) => {
    const blob = call[1] as { parts: string[] };
    return JSON.parse(blob.parts.join("")).event_name as string;
  });
  return [...fromFetch, ...fromBeacon];
}

describe("TempsAnalyticsProvider auto-tracking (ADR 071 D1)", () => {
  beforeEach(() => {
    window.location = {
      ...window.location,
      hostname: "example.com",
      host: "example.com",
      pathname: "/missing",
      search: "",
      protocol: "https:",
      href: "https://example.com/missing",
    };
    vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 204 }));
    (navigator.sendBeacon as unknown as ReturnType<typeof vi.fn>).mockReturnValue(true);
    vi.stubGlobal("Blob", class {
      parts: string[];
      constructor(parts: string[]) {
        this.parts = parts;
      }
    });
    document.body.innerHTML = "";
  });

  it("sends outbound_click by beacon for a link to another host", () => {
    render(
      <TempsAnalyticsProvider autoTrackPageviews={false} autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
        <a href="https://partner.example.org/" onClick={(event) => event.preventDefault()}>
          partner
        </a>
      </TempsAnalyticsProvider>
    );

    document.querySelector("a")?.click();

    expect(sentEventNames()).toEqual(["outbound_click"]);
  });

  it("sends nothing for links when both switches are off", () => {
    render(
      <TempsAnalyticsProvider
        autoTrackPageviews={false}
        autoTrackEngagement={false}
        autoTrackSpeedAnalytics={false}
        autoTrackOutboundLinks={false}
        autoTrackFileDownloads={false}
      >
        <a href="https://partner.example.org/file.zip" onClick={(event) => event.preventDefault()}>
          file
        </a>
      </TempsAnalyticsProvider>
    );

    document.querySelector("a")?.click();

    expect(sentEventNames()).toEqual([]);
  });

  it("reports a page carrying data-temps-404 after its page view", () => {
    render(
      <TempsAnalyticsProvider autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
        <main data-temps-404>Not found</main>
      </TempsAnalyticsProvider>
    );

    expect(sentEventNames()).toEqual(["page_view", "not_found"]);
  });

  it("exposes trackNotFound on the context", async () => {
    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider autoTrackPageviews={false} autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
          {children}
        </TempsAnalyticsProvider>
      ),
    });

    await result.current.trackNotFound({ section: "docs" });

    const fetchMock = global.fetch as unknown as ReturnType<typeof vi.fn>;
    const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
    expect(body.event_name).toBe("not_found");
    expect(body.event_data).toMatchObject({ path: "/missing", section: "docs" });
  });
});
