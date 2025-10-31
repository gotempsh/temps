import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider } from "../Provider";
import { useScrollVisibility } from "../useScrollVisibility";

// Helper function to create a complete IntersectionObserverEntry
function createMockIntersectionObserverEntry(
  target: Element,
  isIntersecting: boolean,
  intersectionRatio = 0.5
): IntersectionObserverEntry {
  return {
    target,
    isIntersecting,
    intersectionRatio,
    boundingClientRect: target.getBoundingClientRect(),
    intersectionRect: target.getBoundingClientRect(),
    rootBounds: null,
    time: Date.now(),
  } as IntersectionObserverEntry;
}

describe("useScrollVisibility", () => {
  let observeMock: ReturnType<typeof vi.fn>;
  let disconnectMock: ReturnType<typeof vi.fn>;
  let IntersectionObserverMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.clearAllMocks();

    // Mock IntersectionObserver
    observeMock = vi.fn();
    disconnectMock = vi.fn();

    IntersectionObserverMock = vi.fn(function (
      this: unknown,
      callback: IntersectionObserverCallback
    ) {
      return {
        observe: observeMock,
        disconnect: disconnectMock,
        unobserve: vi.fn(),
        takeRecords: vi.fn(),
        root: null,
        rootMargin: "0px",
        thresholds: [0.5],
        // Store callback for manual triggering
        __callback: callback,
      };
    });

    global.IntersectionObserver =
      IntersectionObserverMock as unknown as typeof IntersectionObserver;
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("should return a callback ref function", () => {
    const { result } = renderHook(() => useScrollVisibility(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    expect(typeof result.current).toBe("function");
  });

  it("should create an IntersectionObserver with default options", () => {
    const { result } = renderHook(() => useScrollVisibility(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    // Attach a real element to the ref
    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalledWith(
      expect.any(Function),
      expect.objectContaining({
        root: null,
        rootMargin: "0px",
        threshold: 0.5,
      })
    );
  });

  it("should track event when component becomes visible", async () => {
    const fetchSpy = vi
      .spyOn(global, "fetch")
      .mockResolvedValue(new Response("ok", { status: 200 }));

    const { result } = renderHook(
      () =>
        useScrollVisibility({
          eventName: "hero_viewed",
          eventData: { section: "hero" },
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    // Attach element via callback ref
    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalled();

    // Get the observer instance and trigger intersection
    const observerInstance = IntersectionObserverMock.mock.results[0].value;
    const callback = observerInstance.__callback;

    // Simulate element becoming visible
    callback(
      [createMockIntersectionObserverEntry(element, true, 0.75)],
      observerInstance
    );

    await waitFor(() => {
      const heroViewedCall = fetchSpy.mock.calls.find((call) => {
        const body = JSON.parse((call[1] as RequestInit).body as string);
        return body.event_name === "hero_viewed";
      });

      expect(heroViewedCall).toBeDefined();
      const body = JSON.parse(
        (heroViewedCall![1] as RequestInit).body as string
      );
      expect(body.event_data).toEqual({ section: "hero" });
    });
  });

  it("should only track once by default", async () => {
    const fetchSpy = vi
      .spyOn(global, "fetch")
      .mockResolvedValue(new Response("ok", { status: 200 }));

    const { result } = renderHook(
      () =>
        useScrollVisibility({
          eventName: "section_viewed",
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalled();

    const observerInstance = IntersectionObserverMock.mock.results[0].value;
    const callback = observerInstance.__callback;

    // Trigger intersection twice
    callback(
      [createMockIntersectionObserverEntry(element, true)],
      observerInstance
    );
    callback(
      [createMockIntersectionObserverEntry(element, true)],
      observerInstance
    );

    await waitFor(() => {
      const sectionViewedCalls = fetchSpy.mock.calls.filter((call) => {
        const body = JSON.parse((call[1] as RequestInit).body as string);
        return body.event_name === "section_viewed";
      });

      expect(sectionViewedCalls).toHaveLength(1);
    });
  });

  it("should track multiple times when once is false", async () => {
    const fetchSpy = vi
      .spyOn(global, "fetch")
      .mockResolvedValue(new Response("ok", { status: 200 }));

    const { result } = renderHook(
      () =>
        useScrollVisibility({
          eventName: "section_viewed",
          once: false,
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalled();

    const observerInstance = IntersectionObserverMock.mock.results[0].value;
    const callback = observerInstance.__callback;

    // Trigger intersection twice
    callback(
      [createMockIntersectionObserverEntry(element, true)],
      observerInstance
    );
    callback(
      [createMockIntersectionObserverEntry(element, true)],
      observerInstance
    );

    await waitFor(() => {
      const sectionViewedCalls = fetchSpy.mock.calls.filter((call) => {
        const body = JSON.parse((call[1] as RequestInit).body as string);
        return body.event_name === "section_viewed";
      });

      expect(sectionViewedCalls.length).toBeGreaterThan(1);
    });
  });

  it("should not track when element is not intersecting", async () => {
    const fetchSpy = vi
      .spyOn(global, "fetch")
      .mockResolvedValue(new Response("ok", { status: 200 }));

    const { result } = renderHook(
      () =>
        useScrollVisibility({
          eventName: "section_viewed",
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalled();

    const observerInstance = IntersectionObserverMock.mock.results[0].value;
    const callback = observerInstance.__callback;

    // Trigger with not intersecting
    callback(
      [createMockIntersectionObserverEntry(element, false)],
      observerInstance
    );

    // Wait a bit and check no event was tracked
    await new Promise((resolve) => setTimeout(resolve, 100));

    const sectionViewedCalls = fetchSpy.mock.calls.filter((call) => {
      try {
        const body = JSON.parse((call[1] as RequestInit).body as string);
        return body.event_name === "section_viewed";
      } catch {
        return false;
      }
    });

    expect(sectionViewedCalls).toHaveLength(0);
  });

  it("should not track when disabled", async () => {
    vi.spyOn(global, "fetch").mockResolvedValue(
      new Response("ok", { status: 200 })
    );

    const { result } = renderHook(
      () =>
        useScrollVisibility({
          eventName: "section_viewed",
          enabled: false,
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    const element = document.createElement("div");
    result.current(element);

    // Wait a bit to ensure observer is not created
    await new Promise((resolve) => setTimeout(resolve, 100));

    expect(IntersectionObserverMock).not.toHaveBeenCalled();
  });

  it("should not track when analytics provider is disabled", async () => {
    const fetchSpy = vi.spyOn(global, "fetch");

    const { result } = renderHook(
      () =>
        useScrollVisibility({
          eventName: "section_viewed",
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider disabled>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalled();

    const observerInstance = IntersectionObserverMock.mock.results[0].value;
    const callback = observerInstance.__callback;

    callback(
      [createMockIntersectionObserverEntry(element, true)],
      observerInstance
    );

    await new Promise((resolve) => setTimeout(resolve, 100));

    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("should disconnect observer on unmount", () => {
    const { result, unmount } = renderHook(() => useScrollVisibility(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalled();

    unmount();

    expect(disconnectMock).toHaveBeenCalled();
  });

  it("should use custom threshold", () => {
    const { result } = renderHook(
      () =>
        useScrollVisibility({
          threshold: 0.75,
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalledWith(
      expect.any(Function),
      expect.objectContaining({
        threshold: 0.75,
      })
    );
  });

  it("should use custom rootMargin", () => {
    const { result } = renderHook(
      () =>
        useScrollVisibility({
          rootMargin: "100px",
        }),
      {
        wrapper: ({ children }) => (
          <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
        ),
      }
    );

    const element = document.createElement("div");
    result.current(element);

    expect(IntersectionObserverMock).toHaveBeenCalledWith(
      expect.any(Function),
      expect.objectContaining({
        rootMargin: "100px",
      })
    );
  });
});
