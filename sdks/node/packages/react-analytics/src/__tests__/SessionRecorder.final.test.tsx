import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import React, { useState, useEffect } from "react";
import { SessionRecorder } from "../SessionRecorder";

describe("SessionRecorder Final Retry Test", () => {
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
    vi.restoreAllMocks();
  });

  it("should stop trying completely after 3 failures and not retry on prop changes", async () => {
    const logSpy = vi.spyOn(console, "log");
    const errorSpy = vi.spyOn(console, "error");
    const warnSpy = vi.spyOn(console, "warn");

    // All calls will fail
    fetchSpy.mockRejectedValue(new Error("Server unavailable"));

    // Component that changes props periodically
    const TestComponent = () => {
      const [counter, setCounter] = useState(0);

      useEffect(() => {
        const interval = setInterval(() => {
          setCounter(c => c + 1);
        }, 100);

        return () => clearInterval(interval);
      }, []);

      return (
        <SessionRecorder
          enabled={true}
          basePath="/api/_temps"
          domain={`example.com?v=${counter}`} // Changing prop
        />
      );
    };

    render(<TestComponent />);

    // Wait for initial attempts (should be 3)
    await waitFor(() => {
      const attempts = logSpy.mock.calls.filter(call =>
        call[0]?.includes?.("Attempting to initialize session")
      );
      expect(attempts.length).toBeGreaterThan(0);
    }, { timeout: 2000 });

    // Wait longer to ensure no more attempts are made
    await new Promise(resolve => setTimeout(resolve, 1000));

    // Count actual fetch calls
    const initCalls = fetchSpy.mock.calls.filter(
      call => call[0]?.toString?.().includes?.("/session-replay/init") || false
    );

    // Count attempt logs
    const attemptLogs = logSpy.mock.calls.filter(call =>
      call[0]?.includes?.("Attempting to initialize session")
    );

    // Count warning logs about permanent failure
    const permanentFailureLogs = warnSpy.mock.calls.filter(call =>
      call[0]?.includes?.("Initialization has permanently failed") ||
      call[0]?.includes?.("Not starting recording - initialization has permanently failed")
    );

    console.log(`
      Final results after 1 second:
      Init calls: ${initCalls.length}
      Attempt logs: ${attemptLogs.length}
      Permanent failure warnings: ${permanentFailureLogs.length}
    `);

    // Should have made exactly 3 attempts and then stopped
    expect(initCalls.length).toBe(3);
    expect(attemptLogs.length).toBe(3);

    // Verify it logged all 3 attempts properly
    expect(attemptLogs[0][0]).toContain("(attempt 1/3)");
    expect(attemptLogs[1][0]).toContain("(attempt 2/3)");
    expect(attemptLogs[2][0]).toContain("(attempt 3/3)");

    logSpy.mockRestore();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
  });

  it("should allow retrying after component unmount and remount", async () => {
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // All calls will fail
    fetchSpy.mockRejectedValue(new Error("Server error"));

    // First mount - should fail 3 times
    const { unmount } = render(
      <SessionRecorder
        enabled={true}
        basePath="/api/_temps"
        domain="example.com"
      />
    );

    // Wait for 3 attempts
    await waitFor(() => {
      const attempts = logSpy.mock.calls.filter(call =>
        call[0]?.includes?.("Attempting to initialize session")
      );
      if (attempts.length >= 3) {
        return true;
      }
      throw new Error("Waiting for 3 attempts");
    }, { timeout: 2000 });

    const firstMountCalls = fetchSpy.mock.calls.length;

    // Unmount component
    unmount();

    // Clear mocks to track new attempts
    fetchSpy.mockClear();
    logSpy.mockClear();

    // Remount component - should try again with fresh retry count
    render(
      <SessionRecorder
        enabled={true}
        basePath="/api/_temps"
        domain="example.com"
      />
    );

    // Wait a bit for potential initialization
    await new Promise(resolve => setTimeout(resolve, 500));

    const secondMountCalls = fetchSpy.mock.calls.filter(
      call => call[0]?.toString?.().includes?.("/session-replay/init") || false
    );

    const secondAttemptLogs = logSpy.mock.calls.filter(call =>
      call[0]?.includes?.("Attempting to initialize session")
    );

    console.log(`
      First mount: ${firstMountCalls} calls
      Second mount: ${secondMountCalls.length} calls
      Second mount attempts: ${secondAttemptLogs.length}
    `);

    // First mount should have made 3 attempts
    expect(firstMountCalls).toBe(3);

    // Second mount should start fresh and try again
    expect(secondMountCalls.length).toBeGreaterThan(0);

    logSpy.mockRestore();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
  });
});
