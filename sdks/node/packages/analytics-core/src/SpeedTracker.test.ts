// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The speed beacon must measure every visitor, not just the ones who
 * interact, and must attribute each value to the page it was measured on.
 */

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

import { SpeedTracker } from "./SpeedTracker";

function report(name: string, value: number): void {
  for (const callback of reporters[name] ?? []) callback({ value, rating: "good" });
}

let fetchMock: ReturnType<typeof vi.fn>;
let beaconMock: ReturnType<typeof vi.fn>;
let visibility: DocumentVisibilityState;

function fetchBodies(): Array<Record<string, unknown>> {
  return fetchMock.mock.calls.map(([, init]) => JSON.parse((init as RequestInit).body as string));
}

/** jsdom's Blob has no `text()`, so read it the long way. */
function readBlob(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(reader.error);
    reader.readAsText(blob);
  });
}

async function beaconBodies(): Promise<Array<Record<string, unknown>>> {
  return Promise.all(
    beaconMock.mock.calls.map(async ([, blob]) => JSON.parse(await readBlob(blob as Blob)))
  );
}

function setVisibility(state: DocumentVisibilityState): void {
  visibility = state;
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  for (const key of Object.keys(reporters)) delete reporters[key];
  fetchMock = vi.fn().mockResolvedValue({ status: 204, ok: true });
  vi.stubGlobal("fetch", fetchMock);
  beaconMock = vi.fn().mockReturnValue(true);
  Object.defineProperty(navigator, "sendBeacon", { value: beaconMock, configurable: true });
  visibility = "visible";
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => visibility,
  });
  window.history.replaceState({}, "", "/pricing?plan=pro");
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("SpeedTracker load beacon", () => {
  it("sends TTFB, FCP and LCP without waiting for FID", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("TTFB", 120);
    report("FCP", 400);
    expect(fetchMock).not.toHaveBeenCalled();
    report("LCP", 900);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe("/_temps/speed");
    expect(fetchBodies()[0]).toMatchObject({
      ttfb: 120,
      fcp: 400,
      lcp: 900,
      fid: null,
      pathname: "/pricing",
      query: "?plan=pro",
    });
    tracker.destroy();
  });

  it("includes FID when it arrived before the load metrics completed", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("TTFB", 120);
    report("FID", 8);
    report("FCP", 400);
    report("LCP", 900);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchBodies()[0]).toMatchObject({ ttfb: 120, fcp: 400, lcp: 900, fid: 8 });
    tracker.destroy();
  });

  it("sends a late FID on its own instead of re-sending the load metrics", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("TTFB", 120);
    report("FCP", 400);
    report("LCP", 900);
    report("FID", 12);

    expect(fetchMock).toHaveBeenCalledTimes(2);
    const late = fetchBodies()[1];
    expect(late).toMatchObject({ fid: 12, pathname: "/pricing" });
    expect(late).not.toHaveProperty("ttfb");
    tracker.destroy();
  });

  it("flushes what it has by beacon when a visitor leaves before LCP is final", async () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("TTFB", 150);
    report("FCP", 500);

    setVisibility("hidden");

    expect(fetchMock).not.toHaveBeenCalled();
    expect(beaconMock).toHaveBeenCalledTimes(1);
    expect(beaconMock.mock.calls[0][0]).toBe("/_temps/speed");
    expect((await beaconBodies())[0]).toMatchObject({
      ttfb: 150,
      fcp: 500,
      lcp: null,
      fid: null,
      pathname: "/pricing",
    });

    // A later LCP report for the same page must not produce a second row.
    report("LCP", 950);
    expect(beaconMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).not.toHaveBeenCalled();
    tracker.destroy();
  });

  it("sends nothing on hide when no load metric was ever reported", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    setVisibility("hidden");
    window.dispatchEvent(new Event("pagehide"));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(beaconMock).not.toHaveBeenCalled();
    tracker.destroy();
  });

  it("sends the load beacon only once", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("TTFB", 120);
    report("FCP", 400);
    report("LCP", 900);
    report("LCP", 1100);
    window.dispatchEvent(new Event("pagehide"));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(beaconMock).not.toHaveBeenCalled();
    tracker.destroy();
  });
});

describe("SpeedTracker page attribution", () => {
  it("attributes late CLS and INP to the page measured, not the page navigated to", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("TTFB", 120);
    report("FCP", 400);
    report("LCP", 900);

    // Client-side navigation before the late metrics stabilize.
    window.history.pushState({}, "", "/docs/getting-started");
    report("CLS", 0.12);
    report("INP", 180);

    const [load, cls, inp] = fetchBodies();
    expect(load.pathname).toBe("/pricing");
    expect(cls).toMatchObject({ cls: 0.12, pathname: "/pricing", query: "?plan=pro" });
    expect(inp).toMatchObject({ inp: 180, pathname: "/pricing", query: "?plan=pro" });
    tracker.destroy();
  });

  it("uses the `pathname` key the ingest endpoint reads, never `path`", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("CLS", 0.01);
    expect(fetchBodies()[0]).toHaveProperty("pathname", "/pricing");
    expect(fetchBodies()[0]).not.toHaveProperty("path");
    tracker.destroy();
  });

  it("sends late metrics reported while hidden by beacon", async () => {
    const tracker = new SpeedTracker({ basePath: "/_temps", ingestKey: "pa_key" });
    visibility = "hidden";
    report("INP", 64);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(beaconMock.mock.calls[0][0]).toBe("/_temps/speed?temps_key=pa_key");
    expect((await beaconBodies())[0]).toMatchObject({ inp: 64, pathname: "/pricing" });
    tracker.destroy();
  });
});

describe("SpeedTracker.destroy", () => {
  it("ignores reports and page hides after destroy", () => {
    const tracker = new SpeedTracker({ basePath: "/_temps" });
    report("TTFB", 120);
    tracker.destroy();

    report("FCP", 400);
    report("LCP", 900);
    report("CLS", 0.1);
    setVisibility("hidden");

    expect(fetchMock).not.toHaveBeenCalled();
    expect(beaconMock).not.toHaveBeenCalled();
  });
});
