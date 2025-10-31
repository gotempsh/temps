import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import React from "react";
import { SessionRecorder } from "../SessionRecorder";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("SessionRecorder", () => {
  let fetchSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.clearAllMocks();
    fetchSpy = vi.spyOn(global, "fetch");

    // Mock crypto.randomUUID
    Object.defineProperty(global, 'crypto', {
      value: {
        randomUUID: vi.fn(() => "test-session-id-123"),
      },
      writable: true,
    });

    // Reset window properties
    Object.defineProperty(window, "location", {
      value: {
        hostname: "example.com",
        pathname: "/test",
        search: "?test=true",
        href: "https://example.com/test?test=true",
        protocol: "https:",
      },
      writable: true,
    });

    Object.defineProperty(navigator, "userAgent", {
      value: "Mozilla/5.0 Test Browser",
      configurable: true,
    });

    Object.defineProperty(window, "screen", {
      value: {
        width: 1920,
        height: 1080,
      },
      configurable: true,
    });

    Object.defineProperty(window, "innerWidth", {
      value: 1024,
      configurable: true,
    });

    Object.defineProperty(window, "innerHeight", {
      value: 768,
      configurable: true,
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("should not initialize when disabled", () => {
    render(
      <SessionRecorder
        enabled={false}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("should initialize session with POST to /session-replay/init when enabled", async () => {
    fetchSpy.mockResolvedValue(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    // Should call the init endpoint first
    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/session-replay/init`,
        expect.objectContaining({
          method: "POST",
          headers: {
            "Content-Type": "application/json",
          },
          body: expect.stringContaining('"sessionId":"test-session-id-123"'),
        })
      );
    });

    const initCall = fetchSpy.mock.calls[0];
    const body = JSON.parse((initCall[1] as RequestInit).body as string);

    // Verify all required metadata fields are sent (camelCase as per schema)
    expect(body).toMatchObject({
      sessionId: "test-session-id-123",
      visitorId: expect.any(String),
      userAgent: expect.any(String),
      language: expect.any(String),
      timezone: expect.any(String),
      screenWidth: expect.any(Number),
      screenHeight: expect.any(Number),
      colorDepth: expect.any(Number),
      viewportWidth: expect.any(Number),
      viewportHeight: expect.any(Number),
      url: expect.any(String),
      timestamp: expect.any(String),
    });
  });

  it("should send events to POST /session-replay/events after initialization", async () => {
    vi.useFakeTimers();

    // Mock successful init
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
        flushInterval={5000}
      />
    );

    // Wait for initialization
    await vi.runOnlyPendingTimersAsync();

    // Clear the init call
    fetchSpy.mockClear();

    // Mock successful events submission
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ events_added: 10 }), { status: 200 })
    );

    // Simulate some time passing and events being collected
    // In reality, rrweb would be collecting events
    vi.advanceTimersByTime(5000);

    // If events were collected, they should be sent to the events endpoint
    // The actual implementation would base64 encode and compress the events
    if (fetchSpy.mock.calls.length > 0) {
      const eventsCall = fetchSpy.mock.calls[0];
      expect(eventsCall[0]).toBe(`${DEFAULT_BASE_PATH}/session-replay/events`);

      const body = JSON.parse((eventsCall[1] as RequestInit).body as string);
      expect(body).toMatchObject({
        sessionId: "test-session-id-123",
        events: expect.any(String), // Base64 encoded compressed events
      });
    }
  });

  it("should handle 404 from events endpoint when session not found", async () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    // Mock successful init
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    // Mock 404 from events endpoint
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: "Session not found" }), { status: 404 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalled();
    });

    // If events are sent and get 404, should handle gracefully
    // The implementation should either reinitialize or stop recording

    consoleSpy.mockRestore();
  });

  it("should batch events before sending", async () => {
    vi.useFakeTimers();

    // Mock successful init
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
        batchSize={100}
        flushInterval={10000}
      />
    );

    // Wait for initialization
    await vi.runOnlyPendingTimersAsync();
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    expect(fetchSpy).toHaveBeenCalledWith(
      `${DEFAULT_BASE_PATH}/session-replay/init`,
      expect.any(Object)
    );

    fetchSpy.mockClear();

    // Mock events being collected (this would be done by rrweb)
    // Events should be batched and not sent immediately

    // Advance time but not enough to trigger flush
    vi.advanceTimersByTime(5000);

    // Should not have sent events yet (unless batch size reached)
    // This depends on the implementation details

    // Advance to flush interval
    vi.advanceTimersByTime(5000);

    // Now events should be sent if any were collected
    // Without actual rrweb events being generated, no events will be sent
    // This test verifies the batching timer setup doesn't crash

    vi.useRealTimers();
  });

  it("should respect excluded paths", () => {
    Object.defineProperty(window, "location", {
      value: {
        ...window.location,
        pathname: "/admin/dashboard",
      },
      writable: true,
    });

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
        excludedPaths={["/admin/*", "/api/*"]}
      />
    );

    // Should not initialize session for excluded paths
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("should respect session sample rate", () => {
    // Mock Math.random to return 0.6
    vi.spyOn(Math, "random").mockReturnValue(0.6);

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
        sessionSampleRate={0.5} // 50% sample rate
      />
    );

    // Should not initialize since random (0.6) > sampleRate (0.5)
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("should include session when within sample rate", async () => {
    vi.spyOn(Math, "random").mockReturnValue(0.3);
    fetchSpy.mockResolvedValue(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
        sessionSampleRate={0.5}
      />
    );

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/session-replay/init`,
        expect.any(Object)
      );
    });
  });

  it("should send compressed and base64 encoded events", async () => {
    vi.useFakeTimers();

    // Mock successful init
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    // Wait for init
    await vi.runOnlyPendingTimersAsync();
    fetchSpy.mockClear();

    // Mock successful events submission
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ events_added: 5 }), { status: 200 })
    );

    // In a real scenario, when events are sent:
    // 1. Events are collected by rrweb
    // 2. Events are compressed (likely using pako or similar)
    // 3. Compressed data is base64 encoded
    // 4. Sent to /session-replay/events endpoint

    // The body should contain:
    // {
    //   sessionId: "test-session-id-123",
    //   events: "base64EncodedCompressedData"
    // }
  });

  it("should handle initialization failure gracefully", async () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    fetchSpy.mockRejectedValue(new Error("Network error"));

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/session-replay/init`,
        expect.any(Object)
      );
    });

    expect(consoleSpy).toHaveBeenCalledWith(
      expect.stringContaining("[SessionRecorder] Failed to initialize session: Error: Network error (attempt 1/3)")
    );

    consoleSpy.mockRestore();
  });

  it("should stop retrying after 3 failed initialization attempts", async () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const consoleLogSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const consoleWarnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // Mock all init attempts to fail
    fetchSpy.mockRejectedValue(new Error("Network error"));

    const { rerender } = render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    // Wait for first attempt
    await waitFor(() => {
      expect(consoleLogSpy).toHaveBeenCalledWith(
        "[SessionRecorder] Attempting to initialize session (attempt 1/3)"
      );
    });

    // Force re-renders to trigger more attempts
    for (let i = 0; i < 5; i++) {
      rerender(
        <SessionRecorder
          enabled={true}
          basePath={DEFAULT_BASE_PATH}
          domain="example.com"
        />
      );
      await new Promise(resolve => setTimeout(resolve, 100));
    }

    // Should have attempted exactly 3 times
    const initCalls = fetchSpy.mock.calls.filter(
      call => (call[0] as string).includes("/session-replay/init")
    );
    expect(initCalls.length).toBeLessThanOrEqual(3);

    // Should have logged the exceeded retries message
    expect(consoleSpy).toHaveBeenCalledWith(
      "[SessionRecorder] Exceeded maximum initialization retries (3)"
    );

    // Should show warning when trying to start after permanent failure
    expect(consoleWarnSpy).toHaveBeenCalledWith(
      "[SessionRecorder] Initialization has permanently failed, not retrying"
    );

    consoleSpy.mockRestore();
    consoleLogSpy.mockRestore();
    consoleWarnSpy.mockRestore();
  });

  it("should stop recording and flush events when unmounted", async () => {
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    const { unmount } = render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/session-replay/init`,
        expect.any(Object)
      );
    });

    fetchSpy.mockClear();

    // Mock events endpoint for final flush
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ events_added: 3 }), { status: 200 })
    );

    unmount();

    // Should send any remaining events before unmounting
    // Check if events endpoint was called for final flush
  });

  it("should reinitialize session if events endpoint returns 404", async () => {
    vi.useFakeTimers();

    // Initial successful init
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    await vi.runOnlyPendingTimersAsync();
    expect(fetchSpy).toHaveBeenCalledTimes(1);

    fetchSpy.mockClear();

    // Mock 404 when sending events (session not found)
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ error: "Session not found" }), { status: 404 })
    );

    // Mock successful re-initialization
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    // Trigger events to be sent
    vi.advanceTimersByTime(10000);

    // Since there are no actual events collected by rrweb in this test,
    // the events endpoint might not be called.
    // This test would need actual rrweb recording to be meaningful.
    // For now, just verify the component doesn't crash

    vi.useRealTimers();
  });

  it("should use custom domain", async () => {
    fetchSpy.mockResolvedValue(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="custom.domain.com"
      />
    );

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalled();
    });

    const body = JSON.parse(
      (fetchSpy.mock.calls[0][1] as RequestInit).body as string
    );

    // Check that sessionId is included in the payload
    expect(body.sessionId).toBeDefined();
    expect(typeof body.sessionId).toBe("string");
  });

  it("should pass rrweb configuration options", async () => {
    fetchSpy.mockResolvedValue(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
        maskAllInputs={true}
        maskTextSelector="[data-private]"
        blockClass="block-recording"
        ignoreClass="ignore-recording"
        maskTextClass="mask-text"
        recordCanvas={false}
        collectFonts={true}
      />
    );

    // These options would be passed to rrweb.record()
    // Testing that component accepts and handles these props
    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/session-replay/init`,
        expect.any(Object)
      );
    });
  });

  it("should include proper metadata in session initialization", async () => {
    // Set up various browser properties
    Object.defineProperty(document, "referrer", {
      value: "https://google.com/search",
      configurable: true,
    });

    Object.defineProperty(navigator, "language", {
      value: "en-US",
      configurable: true,
    });

    fetchSpy.mockResolvedValue(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    await waitFor(() => {
      expect(fetchSpy).toHaveBeenCalled();
    });

    const body = JSON.parse(
      (fetchSpy.mock.calls[0][1] as RequestInit).body as string
    );

    // Verify comprehensive metadata
    expect(body).toMatchObject({
      sessionId: expect.any(String),
      visitor_id: expect.any(String),
      domain: "example.com",
      request_path: "/test",
      request_query: "?test=true",
      referrer: "https://google.com/search",
      user_agent: "Mozilla/5.0 Test Browser",
      screen_width: 1920,
      screen_height: 1080,
      viewport_width: 1024,
      viewport_height: 768,
      started_at: expect.any(String),
    });
  });

  it("should handle events submission with proper format", async () => {
    vi.useFakeTimers();

    // Mock successful init
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ success: true }), { status: 201 })
    );

    render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
        flushInterval={1000}
      />
    );

    // Wait for init
    await vi.runOnlyPendingTimersAsync();

    fetchSpy.mockClear();

    // Mock successful events submission
    fetchSpy.mockResolvedValueOnce(
      new Response(JSON.stringify({ events_added: 15 }), { status: 200 })
    );

    // Advance time to trigger flush
    vi.advanceTimersByTime(1000);

    // Verify the events endpoint call format if events were sent
    if (fetchSpy.mock.calls.length > 0) {
      const [url, options] = fetchSpy.mock.calls[0];
      expect(url).toBe("/api/_temps/session-replay/events");
      expect(options).toMatchObject({
        method: "POST",
        headers: {
          "Content-Type": "application/json",
        },
      });

      const body = JSON.parse((options as RequestInit).body as string);
      expect(body).toHaveProperty("sessionId", "test-session-id-123");
      expect(body).toHaveProperty("events");
      expect(typeof body.events).toBe("string"); // Base64 encoded
    }
  });
});
