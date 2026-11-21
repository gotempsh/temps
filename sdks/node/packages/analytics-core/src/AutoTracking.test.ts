// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_FILE_DOWNLOAD_EXTENSIONS,
  FILE_DOWNLOAD_EVENT,
  NOT_FOUND_EVENT,
  NOT_FOUND_RECHECK_DELAY_MS,
  OUTBOUND_CLICK_EVENT,
  classifyLink,
  notFoundEventData,
  pathExtension,
  type ClassifyLinkOptions,
  type LinkLike,
} from "./AutoTracking";
import { createAnalytics } from "./Analytics";
import type { AnalyticsApi, AnalyticsOptions } from "./types";

const ALL: ClassifyLinkOptions = {
  trackOutboundLinks: true,
  trackFileDownloads: true,
  fileDownloadExtensions: DEFAULT_FILE_DOWNLOAD_EXTENSIONS,
};

function link(href: string, attributes: string[] = []): LinkLike {
  return { href, hasAttribute: (name: string) => attributes.includes(name) };
}

describe("pathExtension", () => {
  it("returns the lower-cased extension of the last segment", () => {
    expect(pathExtension("/files/Report.PDF")).toBe("pdf");
    expect(pathExtension("/a.b/archive.tar.gz")).toBe("gz");
  });

  it("returns null when the last segment has no extension", () => {
    expect(pathExtension("/docs/")).toBeNull();
    expect(pathExtension("/docs/readme")).toBeNull();
    expect(pathExtension("/a.b/readme")).toBeNull();
    expect(pathExtension("/.env")).toBeNull();
    expect(pathExtension("/file.")).toBeNull();
  });
});

describe("classifyLink", () => {
  it("reports a link to another host as an outbound click", () => {
    expect(classifyLink(link("https://partner.example.org/pricing"), "site.example.com", ALL)).toEqual({
      eventName: OUTBOUND_CLICK_EVENT,
      data: { url: "https://partner.example.org/pricing", host: "partner.example.org" },
    });
  });

  it("ignores links to the current host", () => {
    expect(classifyLink(link("https://site.example.com/about"), "site.example.com", ALL)).toBeNull();
  });

  it("treats another port on the same hostname as another host", () => {
    expect(classifyLink(link("http://site.example.com:8080/"), "site.example.com", ALL)?.eventName).toBe(
      OUTBOUND_CLICK_EVENT
    );
  });

  it("drops the query string and fragment from the recorded URL", () => {
    const result = classifyLink(
      link("https://cdn.example.net/files/report.pdf?X-Amz-Signature=secret#page=2"),
      "site.example.com",
      ALL
    );
    expect(result?.data.url).toBe("https://cdn.example.net/files/report.pdf");
    expect(JSON.stringify(result)).not.toContain("secret");
  });

  it("reports a listed extension as a download, case-insensitively, even on the same host", () => {
    expect(classifyLink(link("https://site.example.com/guide.PDF"), "site.example.com", ALL)).toEqual({
      eventName: FILE_DOWNLOAD_EVENT,
      data: { url: "https://site.example.com/guide.PDF", extension: "pdf", host: "site.example.com" },
    });
  });

  it("prefers a download over an outbound click for a file on another host", () => {
    expect(classifyLink(link("https://cdn.example.net/app.dmg"), "site.example.com", ALL)?.eventName).toBe(
      FILE_DOWNLOAD_EVENT
    );
  });

  it("treats a link with the download attribute as a download whatever its extension", () => {
    expect(classifyLink(link("https://site.example.com/export", ["download"]), "site.example.com", ALL)).toEqual({
      eventName: FILE_DOWNLOAD_EVENT,
      data: { url: "https://site.example.com/export", extension: "", host: "site.example.com" },
    });
  });

  it("ignores extensions that are not listed", () => {
    expect(classifyLink(link("https://site.example.com/page.html"), "site.example.com", ALL)).toBeNull();
  });

  it("replaces the default list when one is given", () => {
    const options = { ...ALL, fileDownloadExtensions: ["STL"] };
    expect(classifyLink(link("https://site.example.com/model.stl"), "site.example.com", options)?.eventName).toBe(
      FILE_DOWNLOAD_EVENT
    );
    expect(classifyLink(link("https://site.example.com/guide.pdf"), "site.example.com", options)).toBeNull();
  });

  it("ignores links that are not http or https", () => {
    for (const href of ["mailto:team@example.com", "tel:+15550100", "javascript:void(0)", "blob:https://site.example.com/uuid"]) {
      expect(classifyLink(link(href), "site.example.com", ALL)).toBeNull();
    }
  });

  it("ignores an unparsable href", () => {
    expect(classifyLink(link("not a url"), "site.example.com", ALL)).toBeNull();
  });

  it("honours each switch on its own", () => {
    const noDownloads = { ...ALL, trackFileDownloads: false };
    expect(classifyLink(link("https://cdn.example.net/app.dmg"), "site.example.com", noDownloads)?.eventName).toBe(
      OUTBOUND_CLICK_EVENT
    );
    const noOutbound = { ...ALL, trackOutboundLinks: false };
    expect(classifyLink(link("https://partner.example.org/"), "site.example.com", noOutbound)).toBeNull();
  });
});

describe("createAnalytics auto-tracking", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  let beaconMock: ReturnType<typeof vi.fn>;
  let analytics: AnalyticsApi | null = null;

  /** Options that switch off everything except what each test turns on. */
  const quiet: AnalyticsOptions = {
    ignoreLocalhost: false,
    autoTrackPageviews: false,
    autoTrackPageLeave: false,
    autoTrackEngagement: false,
    autoTrackSpeedAnalytics: false,
  };

  function start(options: AnalyticsOptions = {}): AnalyticsApi {
    analytics = createAnalytics({ ...quiet, ...options });
    return analytics;
  }

  /** Event names sent through either transport, in order. */
  function sentEvents(): Array<{ event_name: string; event_data?: Record<string, unknown>; request_path?: string }> {
    const fromFetch = fetchMock.mock.calls.map((call) => JSON.parse(call[1].body as string));
    const fromBeacon = beaconMock.mock.calls.map((call) => JSON.parse(String((call[1] as { text: string }).text)));
    return [...fromFetch, ...fromBeacon];
  }

  function addLink(href: string): HTMLAnchorElement {
    const anchor = document.createElement("a");
    anchor.href = href;
    anchor.textContent = "link";
    // jsdom cannot navigate; stop it from trying.
    anchor.addEventListener("click", (event) => event.preventDefault());
    document.body.appendChild(anchor);
    return anchor;
  }

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue({ status: 204, ok: true });
    vi.stubGlobal("fetch", fetchMock);
    // A Blob's text is async; capture the payload synchronously instead.
    beaconMock = vi.fn((_url: string, blob: Blob) => {
      void blob;
      return true;
    });
    vi.stubGlobal("Blob", class {
      text: string;
      type: string;
      constructor(parts: string[], options: { type: string }) {
        this.text = parts.join("");
        this.type = options.type;
      }
    });
    Object.defineProperty(navigator, "sendBeacon", { value: beaconMock, configurable: true });
    document.body.innerHTML = "";
  });

  afterEach(() => {
    analytics?.destroy();
    analytics = null;
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("sends outbound_click by beacon when an outbound link is clicked", () => {
    start();
    addLink("https://partner.example.org/pricing?ref=nav").click();

    expect(beaconMock).toHaveBeenCalledTimes(1);
    const [event] = sentEvents();
    expect(event.event_name).toBe(OUTBOUND_CLICK_EVENT);
    expect(event.event_data).toMatchObject({ url: "https://partner.example.org/pricing", host: "partner.example.org" });
    expect(event.request_path).toBe(window.location.pathname);
  });

  it("sends file_download for a click inside a link to a listed file", () => {
    start();
    const anchor = addLink(`${window.location.origin}/files/manual.pdf`);
    const icon = document.createElement("span");
    anchor.appendChild(icon);
    icon.click();

    const [event] = sentEvents();
    expect(event.event_name).toBe(FILE_DOWNLOAD_EVENT);
    expect(event.event_data).toMatchObject({ extension: "pdf" });
  });

  it("counts a middle click but not a right click", () => {
    start();
    const anchor = addLink("https://partner.example.org/");
    anchor.dispatchEvent(new MouseEvent("auxclick", { bubbles: true, button: 2 }));
    expect(beaconMock).not.toHaveBeenCalled();
    anchor.dispatchEvent(new MouseEvent("auxclick", { bubbles: true, button: 1 }));
    expect(beaconMock).toHaveBeenCalledTimes(1);
  });

  it("sends nothing for internal links", () => {
    start();
    addLink(`${window.location.origin}/about`).click();
    expect(beaconMock).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("sends nothing when both link switches are off", () => {
    start({ autoTrackOutboundLinks: false, autoTrackFileDownloads: false });
    addLink("https://partner.example.org/").click();
    addLink("https://cdn.example.net/app.zip").click();
    expect(beaconMock).not.toHaveBeenCalled();
  });

  it("stops listening after destroy()", () => {
    start().destroy();
    analytics = null;
    addLink("https://partner.example.org/").click();
    expect(beaconMock).not.toHaveBeenCalled();
  });

  it("sends nothing when analytics is disabled", () => {
    start({ disabled: true });
    addLink("https://partner.example.org/").click();
    expect(beaconMock).not.toHaveBeenCalled();
  });

  it("trackNotFound() sends not_found with the current path", async () => {
    await start().trackNotFound({ section: "docs" });
    const [event] = sentEvents();
    expect(event.event_name).toBe(NOT_FOUND_EVENT);
    expect(event.event_data).toMatchObject({ path: window.location.pathname, section: "docs" });
  });

  it("reports a page carrying the 404 marker once, after its page view", () => {
    document.body.innerHTML = "<main data-temps-404>Not found</main>";
    start({ autoTrackPageviews: true });

    expect(sentEvents().map((event) => event.event_name)).toEqual(["page_view", NOT_FOUND_EVENT]);
  });

  it("reports a 404 view that renders after the page view", () => {
    vi.useFakeTimers();
    start({ autoTrackPageviews: true });
    expect(sentEvents().map((event) => event.event_name)).toEqual(["page_view"]);

    document.body.innerHTML = "<div data-temps-404></div>";
    vi.advanceTimersByTime(NOT_FOUND_RECHECK_DELAY_MS);
    expect(sentEvents().map((event) => event.event_name)).toEqual(["page_view", NOT_FOUND_EVENT]);

    vi.advanceTimersByTime(NOT_FOUND_RECHECK_DELAY_MS * 5);
    expect(sentEvents()).toHaveLength(2);
  });

  it("does not report pages without the marker", () => {
    vi.useFakeTimers();
    start({ autoTrackPageviews: true });
    vi.advanceTimersByTime(NOT_FOUND_RECHECK_DELAY_MS * 2);
    expect(sentEvents().map((event) => event.event_name)).toEqual(["page_view"]);
  });

  it("does not report the next page when navigating away from a 404 view", () => {
    vi.useFakeTimers();
    document.body.innerHTML = "<main data-temps-404>Not found</main>";
    start({ autoTrackPageviews: true });
    expect(sentEvents().map((event) => event.event_name)).toEqual(["page_view", NOT_FOUND_EVENT]);

    // The router changes the URL first; the old view is still in the DOM.
    window.history.pushState({}, "", "/pricing");
    document.body.innerHTML = "<main>Pricing</main>";
    vi.advanceTimersByTime(NOT_FOUND_RECHECK_DELAY_MS * 2);

    expect(sentEvents().map((event) => event.event_name)).toEqual([
      "page_view",
      NOT_FOUND_EVENT,
      "page_view",
    ]);
  });

  it("reports a 404 view an in-app navigation renders", () => {
    vi.useFakeTimers();
    start({ autoTrackPageviews: true });
    window.history.pushState({}, "", "/missing-page");
    document.body.innerHTML = "<main data-temps-404>Not found</main>";
    vi.advanceTimersByTime(NOT_FOUND_RECHECK_DELAY_MS);

    const events = sentEvents();
    expect(events.map((event) => event.event_name)).toEqual(["page_view", "page_view", NOT_FOUND_EVENT]);
    expect(events[2].event_data).toMatchObject({ path: "/missing-page" });
  });

  it("reports a 404 view rendered before a manually recorded page view", () => {
    vi.useFakeTimers();
    const api = start({ autoTrackPageviews: false });
    api.trackPageview();
    vi.advanceTimersByTime(NOT_FOUND_RECHECK_DELAY_MS);

    // The router renders the new route, then its effect records the view.
    window.history.pushState({}, "", "/missing-page");
    document.body.innerHTML = "<main data-temps-404>Not found</main>";
    api.trackPageview();

    const events = sentEvents();
    expect(events.map((event) => event.event_name)).toEqual(["page_view", "page_view", NOT_FOUND_EVENT]);
    expect(events[2].event_data).toMatchObject({ path: "/missing-page" });
  });

  it("ignores the marker when autoTrackNotFound is off", () => {
    document.body.innerHTML = "<main data-temps-404></main>";
    start({ autoTrackPageviews: true, autoTrackNotFound: false });
    expect(sentEvents().map((event) => event.event_name)).toEqual(["page_view"]);
  });
});

describe("trackNotFound outside an enabled browser page", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("reads nothing from the page when analytics is off", async () => {
    const referrer = vi.spyOn(document, "referrer", "get");
    const analytics = createAnalytics({ disabled: true, autoTrackPageviews: false });
    await analytics.trackNotFound({ section: "docs" });
    expect(referrer).not.toHaveBeenCalled();
    analytics.destroy();
  });

  it("describes no page without a window, as during server rendering", () => {
    vi.stubGlobal("window", undefined);
    expect(notFoundEventData({ section: "docs" })).toEqual({ section: "docs" });
  });
});
