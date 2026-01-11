import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider } from "../Provider";
import { useTrackEvent } from "../useTrackEvent";

describe("useTrackEvent", () => {
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

  it("should return a track event function", () => {
    const { result } = renderHook(() => useTrackEvent(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    expect(typeof result.current).toBe("function");
  });

  it("should track events when called", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(() => useTrackEvent(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current("button_click", { button: "submit" });

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.objectContaining({
          method: "POST",
          headers: {
            "Content-Type": "application/json",
          },
          body: expect.stringContaining('"event_name":"button_click"'),
        })
      );
    });

    // Find the button_click event (might be second if page_view was tracked first)
    const buttonClickCall = fetchSpy.mock.calls.find(call => {
      const body = JSON.parse((call[1] as RequestInit).body as string);
      return body.event_name === "button_click";
    });

    expect(buttonClickCall).toBeDefined();
    const body = JSON.parse((buttonClickCall![1] as RequestInit).body as string);
    expect(body.event_data).toEqual({ button: "submit" });
    expect(body.event_name).toBe("button_click");
  });

  it("should work without event data", async () => {
    const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(() => useTrackEvent(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current("page_scroll");

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        "/api/_temps/event",
        expect.objectContaining({
          body: expect.stringContaining('"event_name":"page_scroll"'),
        })
      );
    });
  });

  it("should not track when analytics is disabled", async () => {
    const fetchSpy = vi.spyOn(global, "fetch");

    const { result } = renderHook(() => useTrackEvent(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider disabled>{children}</TempsAnalyticsProvider>
      ),
    });

    await result.current("test_event");

    expect(fetchSpy).not.toHaveBeenCalled();
  });
});
