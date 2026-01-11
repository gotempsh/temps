import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import React from "react";
import { SessionRecorder } from "../SessionRecorder";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("SessionRecorder Retry Logic", () => {
  let fetchSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.clearAllMocks();
    fetchSpy = vi.spyOn(global, "fetch");

    // Mock crypto.randomUUID
    Object.defineProperty(global, 'crypto', {
      value: {
        randomUUID: vi.fn(() => `test-session-${Date.now()}`),
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
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("should only attempt initialization 3 times on failure", async () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const consoleLogSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const consoleWarnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // Mock all init attempts to fail with 404
    let initCallCount = 0;
    fetchSpy.mockImplementation(async (url) => {
      if ((url as string).includes("/session-replay/init")) {
        initCallCount++;
        return new Response(JSON.stringify({ error: "Not found" }), { status: 404 });
      }
      return new Response("ok", { status: 200 });
    });

    // Create a single instance that will attempt initialization 3 times through rerenders
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

    // Trigger more attempts through rerenders
    for (let i = 0; i < 4; i++) {
      rerender(
        <SessionRecorder
          enabled={true}
          basePath={DEFAULT_BASE_PATH}
          domain="example.com"
        />
      );
      await new Promise(resolve => setTimeout(resolve, 50));
    }

    // Verify it attempted exactly 3 times for this single instance
    expect(initCallCount).toBeLessThanOrEqual(3);

    // After 3 attempts, should see the exceeded retries message
    expect(consoleSpy).toHaveBeenCalledWith(
      "[SessionRecorder] Exceeded maximum initialization retries (3)"
    );

    consoleSpy.mockRestore();
    consoleLogSpy.mockRestore();
    consoleWarnSpy.mockRestore();
  });

  it("should stop trying after max retries are exceeded", async () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const consoleWarnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // Mock all init attempts to fail
    let initCallCount = 0;
    fetchSpy.mockImplementation(async (url) => {
      if ((url as string).includes("/session-replay/init")) {
        initCallCount++;
        return new Response(JSON.stringify({ error: "Server error" }), { status: 500 });
      }
      return new Response("ok", { status: 200 });
    });

    // Create a single instance
    const { rerender } = render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    // Trigger multiple rerenders to attempt retries
    for (let i = 0; i < 10; i++) {
      rerender(
        <SessionRecorder
          enabled={true}
          basePath={DEFAULT_BASE_PATH}
          domain="example.com"
        />
      );
      await new Promise(resolve => setTimeout(resolve, 50));
    }

    // Should not exceed 3 attempts for this single instance
    expect(initCallCount).toBeLessThanOrEqual(3);

    // After 3 attempts, should see the exceeded retries message
    expect(consoleSpy).toHaveBeenCalledWith(
      "[SessionRecorder] Exceeded maximum initialization retries (3)"
    );

    // Should also see permanent failure warning
    expect(consoleWarnSpy).toHaveBeenCalledWith(
      "[SessionRecorder] Initialization has permanently failed, not retrying"
    );

    consoleSpy.mockRestore();
    consoleWarnSpy.mockRestore();
  });
});
