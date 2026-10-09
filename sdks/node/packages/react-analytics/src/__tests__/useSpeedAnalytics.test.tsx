// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

type Reporter = (metric: { value: number; rating: string }) => void;
const reporters: Record<string, Reporter[]> = {};

function subscribe(name: string) {
  return (callback: Reporter) => {
    (reporters[name] ??= []).push(callback);
  };
}

vi.mock("web-vitals", () => ({
  onTTFB: subscribe("TTFB"),
  onFCP: subscribe("FCP"),
  onLCP: subscribe("LCP"),
  onFID: subscribe("FID"),
  onCLS: subscribe("CLS"),
  onINP: subscribe("INP"),
}));

import { useSpeedAnalytics } from "../useSpeedAnalytics";

function report(name: string, value: number): void {
  for (const callback of reporters[name] ?? []) callback({ value, rating: "good" });
}

function speedBodies(): Array<Record<string, unknown>> {
  return vi
    .mocked(global.fetch)
    .mock.calls.filter(([url]) => String(url).endsWith("/speed"))
    .map(([, init]) => JSON.parse((init as RequestInit).body as string));
}

describe("useSpeedAnalytics", () => {
  beforeEach(() => {
    for (const key of Object.keys(reporters)) delete reporters[key];
    vi.mocked(global.fetch).mockResolvedValue(new Response(null, { status: 204 }));
    window.location.pathname = "/test";
    window.location.search = "?test=true";
  });

  it("measures visitors who never interact", () => {
    const { unmount } = renderHook(() => useSpeedAnalytics({ basePath: "/_temps" }));
    report("TTFB", 110);
    report("FCP", 350);
    report("LCP", 800);

    expect(speedBodies()).toEqual([
      expect.objectContaining({
        ttfb: 110,
        fcp: 350,
        lcp: 800,
        fid: null,
        pathname: "/test",
        query: "?test=true",
      }),
    ]);
    unmount();
  });

  it("keeps the mount-time path for late metrics after a client-side navigation", () => {
    const { unmount } = renderHook(() => useSpeedAnalytics({ basePath: "/_temps" }));
    window.location.pathname = "/other";
    window.location.search = "";
    report("CLS", 0.2);
    report("INP", 240);

    expect(speedBodies()).toEqual([
      expect.objectContaining({ cls: 0.2, pathname: "/test", query: "?test=true" }),
      expect.objectContaining({ inp: 240, pathname: "/test", query: "?test=true" }),
    ]);
    unmount();
  });

  it("stops sending after unmount", () => {
    const { unmount } = renderHook(() => useSpeedAnalytics({ basePath: "/_temps" }));
    unmount();
    report("TTFB", 110);
    report("FCP", 350);
    report("LCP", 800);
    report("CLS", 0.1);
    expect(speedBodies()).toEqual([]);
  });

  it("does not subscribe when disabled", () => {
    const { unmount } = renderHook(() =>
      useSpeedAnalytics({ basePath: "/_temps", disabled: true })
    );
    expect(reporters.TTFB).toBeUndefined();
    unmount();
  });
});
