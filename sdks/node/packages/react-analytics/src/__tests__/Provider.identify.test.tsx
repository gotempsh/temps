// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider, useTempsAnalytics } from "../Provider";
import type { AnalyticsContextValue } from "../types";

describe("TempsAnalyticsProvider identify (ADR 109 D4)", () => {
  let api: AnalyticsContextValue | null = null;

  function Capture(): null {
    api = useTempsAnalytics();
    return null;
  }

  beforeEach(() => {
    api = null;
    window.location = {
      ...window.location,
      hostname: "example.com",
      host: "example.com",
      pathname: "/signup",
      search: "",
      protocol: "https:",
      href: "https://example.com/signup",
    };
    vi.spyOn(global, "fetch").mockImplementation(async (input) =>
      String(input).endsWith("/identify")
        ? new Response(JSON.stringify({ applied: true, outcome: "updated", message: "ok" }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          })
        : new Response(null, { status: 204 })
    );
    localStorage.clear();
  });

  it("identifies after the first page view and resets on sign-out", async () => {
    render(
      <TempsAnalyticsProvider autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
        <Capture />
      </TempsAnalyticsProvider>
    );
    expect(api).not.toBeNull();
    const fetchMock = global.fetch as unknown as ReturnType<typeof vi.fn>;

    let result: Awaited<ReturnType<AnalyticsContextValue["identify"]>> | undefined;
    await act(async () => {
      result = await api?.identify("user-42", { plan: "pro" });
    });
    expect(result).toMatchObject({ applied: true, outcome: "updated" });

    const urls = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(urls.indexOf("/api/_temps/event")).toBeLessThan(urls.indexOf("/api/_temps/identify"));
    const identifyCall = fetchMock.mock.calls.find((call) => String(call[0]).endsWith("/identify"));
    const body = JSON.parse(identifyCall?.[1]?.body as string) as Record<string, unknown>;
    expect(body).toMatchObject({ user_id: "user-42", traits: { plan: "pro" } });
    expect(body.visitor_id).toBe(api?.getVisitorId());

    await act(async () => {
      await api?.reset();
    });
    expect(api?.getVisitorId()).toBeUndefined();
    expect(fetchMock.mock.calls.some((call) => String(call[0]).endsWith("/reset"))).toBe(true);
  });

  it("sends no identify in cookieless mode", async () => {
    render(
      <TempsAnalyticsProvider cookieless autoTrackEngagement={false} autoTrackSpeedAnalytics={false}>
        <Capture />
      </TempsAnalyticsProvider>
    );
    let result: Awaited<ReturnType<AnalyticsContextValue["identify"]>> | undefined;
    await act(async () => {
      result = await api?.identify("user-42");
    });
    expect(result).toMatchObject({ applied: false, outcome: "cookieless_project" });
    const fetchMock = global.fetch as unknown as ReturnType<typeof vi.fn>;
    expect(fetchMock.mock.calls.some((call) => String(call[0]).endsWith("/identify"))).toBe(false);
  });
});
