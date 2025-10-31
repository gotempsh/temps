import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import React from "react";
import { SessionRecorder } from "../SessionRecorder";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("SessionRecorder Single Instance Retry", () => {
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

    // Don't suppress console output - we want to see it

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
    vi.restoreAllMocks();
  });

  it("should respect the 3 retry limit within a single component instance", async () => {
    // Track all console.log calls to see attempts
    const logSpy = vi.spyOn(console, "log");
    const errorSpy = vi.spyOn(console, "error");

    // First call fails, triggering retry logic
    fetchSpy
      .mockRejectedValueOnce(new Error("Network error 1"))
      .mockRejectedValueOnce(new Error("Network error 2"))
      .mockRejectedValueOnce(new Error("Network error 3"))
      .mockRejectedValueOnce(new Error("Network error 4")); // This shouldn't be reached

    const { rerender } = render(
      <SessionRecorder
        enabled={true}
        basePath={DEFAULT_BASE_PATH}
        domain="example.com"
      />
    );

    // Wait for first attempt
    await waitFor(() => {
      const attempts = logSpy.mock.calls.filter(call =>
        call[0]?.includes?.("Attempting to initialize session")
      );
      expect(attempts.length).toBeGreaterThan(0);
    });

    // Manually trigger more initialization attempts by changing props slightly
    // This simulates the component trying again (e.g., after a state change)
    for (let i = 0; i < 5; i++) {
      rerender(
        <SessionRecorder
          enabled={true}
          basePath={DEFAULT_BASE_PATH}
          domain={`example.com`} // Same value, but causes re-render
        />
      );
      await new Promise(resolve => setTimeout(resolve, 50));
    }

    // Count actual fetch calls
    const initCalls = fetchSpy.mock.calls.filter(
      call => call[0]?.toString?.().includes?.("/session-replay/init") || false
    );

    // Count logged attempts
    const attemptLogs = logSpy.mock.calls.filter(call =>
      call[0]?.includes?.("Attempting to initialize session")
    );

    // Count error logs
    const errorLogs = errorSpy.mock.calls.filter(call =>
      call[0]?.includes?.("Failed to initialize session")
    );

    const exceededLogs = errorSpy.mock.calls.filter(call =>
      call[0]?.includes?.("Exceeded maximum initialization retries")
    );

    console.log(`
      Init fetch calls: ${initCalls.length}
      Attempt logs: ${attemptLogs.length}
      Error logs: ${errorLogs.length}
      Exceeded logs: ${exceededLogs.length}

      Attempt messages: ${attemptLogs.map(c => c[0]).join(', ')}
    `);

    // Should make exactly 3 attempts
    expect(initCalls.length).toBe(3);

    // Should have logged each attempt
    expect(attemptLogs.length).toBe(3);
    expect(attemptLogs[0][0]).toContain("(attempt 1/3)");
    expect(attemptLogs[1][0]).toContain("(attempt 2/3)");
    expect(attemptLogs[2][0]).toContain("(attempt 3/3)");

    // Should have logged failures
    expect(errorLogs.length).toBeGreaterThan(0);
  });
});
