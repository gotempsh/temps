import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider } from "../Provider";
import { useTrackPageview } from "../useTrackPageview";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("useTrackPageview", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.location = {
      ...window.location,
      hostname: "example.com",
      pathname: "/test",
      search: "?test=true",
      protocol: "https:",
    };
  });

  it("should return a track pageview function", () => {
    const { result } = renderHook(() => useTrackPageview(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    expect(typeof result.current).toBe("function");
  });

  it("should track pageviews when called", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(() => useTrackPageview(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current();

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.objectContaining({
          method: "POST",
          headers: {
            "Content-Type": "application/json",
          },
          body: expect.stringContaining('"event_name":"page_view"'),
        })
      );
    });

    const body = JSON.parse(
      (fetchSpy.mock.calls[0][1] as RequestInit).body as string
    );
    expect(body.request_query).toBe("?test=true");
    expect(body.domain).toBe("example.com");
  });

  it("should include referrer and user agent", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    Object.defineProperty(document, "referrer", {
      value: "https://google.com",
      configurable: true,
    });

    Object.defineProperty(navigator, "userAgent", {
      value: "Mozilla/5.0 Test Browser",
      configurable: true,
    });

    const { result } = renderHook(() => useTrackPageview(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current();

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalled();
    });

    const body = JSON.parse(
      (fetchSpy.mock.calls[0][1] as RequestInit).body as string
    );
    expect(body.event_data.referrer).toBe("https://google.com");
    expect(body.event_data.userAgent).toBe("Mozilla/5.0 Test Browser");
  });

  it("should not track when analytics is disabled", async () => {
    const fetchSpy = vi.spyOn(global, "fetch");

    const { result } = renderHook(() => useTrackPageview(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider disabled>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current();

    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
