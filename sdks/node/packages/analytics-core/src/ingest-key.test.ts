// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Coverage for the ADR-040 analytics ingest key across every transport the SDK
 * uses.
 *
 * The two rules being pinned down:
 *
 * 1. `fetch` carries the key as `X-Temps-Analytics-Key`; `navigator.sendBeacon`
 *    carries it as `?temps_key=` because it cannot set headers at all.
 * 2. With no `ingestKey` configured, requests go out byte-identically to before
 *    the feature existed — no header, no query param — so Temps-hosted apps
 *    keep resolving by `Host`.
 */

const recordCalls: Array<{ emit: (event: unknown) => void; stop: ReturnType<typeof vi.fn> }> = [];

vi.mock("rrweb", () => ({
  record: (options: { emit: (event: unknown) => void }) => {
    const stop = vi.fn();
    recordCalls.push({ emit: options.emit, stop });
    return stop;
  },
}));

vi.mock("@rrweb/packer", () => ({
  pack: (events: unknown) => JSON.stringify(events),
}));

import { INGEST_KEY_HEADER } from "./constants";
import { SessionRecorder } from "./SessionRecorder";
import {
  ingestKeyHeaders,
  sendAnalytics,
  sendAnalyticsReliable,
  withIngestKey,
} from "./utils";

/** A key with a character that must survive URL encoding. */
const KEY = "pa_deadbeef+cafe";
const ENCODED_KEY = "pa_deadbeef%2Bcafe";

let fetchMock: ReturnType<typeof vi.fn>;
let beaconMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  recordCalls.length = 0;
  fetchMock = vi.fn().mockResolvedValue({ status: 201, ok: true });
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
});

/** Headers of the nth fetch call, normalized to a plain record. */
function fetchHeaders(index = 0): Record<string, string> {
  const init = fetchMock.mock.calls[index]?.[1] as { headers?: Record<string, string> };
  return init?.headers ?? {};
}

function fetchUrl(index = 0): string {
  return String(fetchMock.mock.calls[index]?.[0]);
}

/** Install a sendBeacon that reports successful queueing. */
function stubBeacon(): void {
  beaconMock = vi.fn().mockReturnValue(true);
  vi.stubGlobal("navigator", {
    userAgent: "test",
    language: "en",
    sendBeacon: beaconMock,
  });
}

describe("withIngestKey", () => {
  it("returns the url untouched when no key is configured", () => {
    expect(withIngestKey("/api/_temps/event")).toBe("/api/_temps/event");
    expect(withIngestKey("/api/_temps/event", "")).toBe("/api/_temps/event");
  });

  it("appends the key with ? on a url that has no query string", () => {
    expect(withIngestKey("/api/_temps/event", "pa_abc")).toBe(
      "/api/_temps/event?temps_key=pa_abc",
    );
  });

  it("appends the key with & on a url that already has a query string", () => {
    expect(withIngestKey("/api/_temps/event?a=1", "pa_abc")).toBe(
      "/api/_temps/event?a=1&temps_key=pa_abc",
    );
  });

  it("url-encodes the key", () => {
    expect(withIngestKey("/e", KEY)).toBe(`/e?temps_key=${ENCODED_KEY}`);
  });
});

describe("ingestKeyHeaders", () => {
  it("is empty when no key is configured", () => {
    expect(ingestKeyHeaders()).toEqual({});
    expect(ingestKeyHeaders("")).toEqual({});
  });

  it("carries the key under X-Temps-Analytics-Key", () => {
    expect(ingestKeyHeaders("pa_abc")).toEqual({ "X-Temps-Analytics-Key": "pa_abc" });
  });
});

describe("sendAnalytics (fetch transport)", () => {
  it("attaches the ingest key as a header", async () => {
    await sendAnalytics("event", { event_name: "x" }, "POST", "/api/_temps", KEY);

    expect(fetchHeaders()[INGEST_KEY_HEADER]).toBe(KEY);
    expect(fetchHeaders()["Content-Type"]).toBe("application/json");
  });

  it("keeps the key out of the url — a header transport has no reason to leak it", async () => {
    await sendAnalytics("event", { event_name: "x" }, "POST", "/api/_temps", KEY);

    expect(fetchUrl()).toBe("/api/_temps/event");
  });

  it("sends no key header and no query param when none is configured", async () => {
    await sendAnalytics("event", { event_name: "x" }, "POST", "/api/_temps");

    expect(fetchHeaders()).toEqual({ "Content-Type": "application/json" });
    expect(fetchUrl()).toBe("/api/_temps/event");
  });

  it("applies to the speed endpoint too", async () => {
    await sendAnalytics("speed", { lcp: 1 }, "POST", "/api/_temps", KEY);

    expect(fetchUrl()).toBe("/api/_temps/speed");
    expect(fetchHeaders()[INGEST_KEY_HEADER]).toBe(KEY);
  });

  it("applies to the speed/update endpoint too", async () => {
    await sendAnalytics("speed/update", { cls: 1 }, "POST", "/api/_temps", KEY);

    expect(fetchUrl()).toBe("/api/_temps/speed/update");
    expect(fetchHeaders()[INGEST_KEY_HEADER]).toBe(KEY);
  });
});

describe("sendAnalyticsReliable (sendBeacon transport)", () => {
  it("appends the url-encoded key as ?temps_key= — sendBeacon cannot set headers", () => {
    stubBeacon();

    sendAnalyticsReliable("event", { event_name: "page_leave" }, "/api/_temps", KEY);

    expect(beaconMock).toHaveBeenCalledTimes(1);
    expect(String(beaconMock.mock.calls[0][0])).toBe(
      `/api/_temps/event?temps_key=${ENCODED_KEY}`,
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("sends a bare url when no key is configured", () => {
    stubBeacon();

    sendAnalyticsReliable("event", { event_name: "page_leave" }, "/api/_temps");

    expect(String(beaconMock.mock.calls[0][0])).toBe("/api/_temps/event");
  });

  it("carries the key on the keepalive fetch fallback when sendBeacon is unavailable", () => {
    vi.stubGlobal("navigator", { userAgent: "test", language: "en", sendBeacon: undefined });

    sendAnalyticsReliable("event", { event_name: "page_leave" }, "/api/_temps", KEY);

    // The fallback shares the beacon url: a custom header here would force a
    // CORS preflight during unload, which browsers routinely drop.
    expect(fetchUrl()).toBe(`/api/_temps/event?temps_key=${ENCODED_KEY}`);
    expect(fetchHeaders()[INGEST_KEY_HEADER]).toBeUndefined();
  });

  it("leaves the fallback url untouched when no key is configured", () => {
    vi.stubGlobal("navigator", { userAgent: "test", language: "en", sendBeacon: undefined });

    sendAnalyticsReliable("event", { event_name: "page_leave" }, "/api/_temps");

    expect(fetchUrl()).toBe("/api/_temps/event");
  });
});

describe("SessionRecorder ingest key", () => {
  function initCalls(): number[] {
    return fetchMock.mock.calls
      .map((call, index) => ({ url: String(call[0]), index }))
      .filter(({ url }) => url.split("?")[0].endsWith("/session-replay/init"))
      .map(({ index }) => index);
  }

  it("sends the key as a header on session init (fetch transport)", async () => {
    vi.stubGlobal("navigator", { userAgent: "test", language: "en", sendBeacon: undefined });
    const recorder = new SessionRecorder({ enabled: true, ingestKey: KEY });

    await vi.waitFor(() => expect(initCalls()).toHaveLength(1));

    const index = initCalls()[0];
    expect(fetchUrl(index)).toBe("/api/_temps/session-replay/init");
    expect(fetchHeaders(index)[INGEST_KEY_HEADER]).toBe(KEY);

    recorder.destroy();
  });

  it("sends no key header on session init when none is configured", async () => {
    vi.stubGlobal("navigator", { userAgent: "test", language: "en", sendBeacon: undefined });
    const recorder = new SessionRecorder({ enabled: true });

    await vi.waitFor(() => expect(initCalls()).toHaveLength(1));

    expect(fetchHeaders(initCalls()[0])[INGEST_KEY_HEADER]).toBeUndefined();

    recorder.destroy();
  });

  it("sends the key as a header on the batched events fetch", async () => {
    vi.stubGlobal("navigator", { userAgent: "test", language: "en", sendBeacon: undefined });
    const recorder = new SessionRecorder({ enabled: true, ingestKey: KEY, batchSize: 1 });
    await vi.waitFor(() => expect(recordCalls).toHaveLength(1));

    recordCalls[0].emit({ type: 3, timestamp: 1, id: 1 });

    await vi.waitFor(() => {
      const posts = fetchMock.mock.calls.filter(([url]) =>
        String(url).split("?")[0].endsWith("/session-replay/events"),
      );
      expect(posts).toHaveLength(1);
      expect((posts[0][1] as { headers: Record<string, string> }).headers[INGEST_KEY_HEADER]).toBe(
        KEY,
      );
      // The batched path is a plain fetch, so the credential stays out of the url.
      expect(String(posts[0][0])).toBe("/api/_temps/session-replay/events");
    });

    recorder.destroy();
  });

  it("appends the key to the url on the unload flush (sendBeacon transport)", async () => {
    stubBeacon();
    const recorder = new SessionRecorder({ enabled: true, ingestKey: KEY, batchSize: 100000 });
    await vi.waitFor(() => expect(recordCalls).toHaveLength(1));

    recordCalls[0].emit({ type: 3, timestamp: 1, id: 1 });
    // destroy() flushes what is buffered down the reliable path.
    recorder.destroy();

    await vi.waitFor(() => {
      const beacons = beaconMock.mock.calls.filter(([url]) =>
        String(url).split("?")[0].endsWith("/session-replay/events"),
      );
      expect(beacons).toHaveLength(1);
      expect(String(beacons[0][0])).toBe(
        `/api/_temps/session-replay/events?temps_key=${ENCODED_KEY}`,
      );
    });
  });

  it("leaves the unload flush url bare when no key is configured", async () => {
    stubBeacon();
    const recorder = new SessionRecorder({ enabled: true, batchSize: 100000 });
    await vi.waitFor(() => expect(recordCalls).toHaveLength(1));

    recordCalls[0].emit({ type: 3, timestamp: 1, id: 1 });
    recorder.destroy();

    await vi.waitFor(() => {
      const beacons = beaconMock.mock.calls.filter(([url]) =>
        String(url).endsWith("/session-replay/events"),
      );
      expect(beacons).toHaveLength(1);
    });
  });
});
