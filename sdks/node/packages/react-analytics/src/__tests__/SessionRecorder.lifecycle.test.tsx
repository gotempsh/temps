// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import { SessionRecorder } from "../SessionRecorder";
import { DEFAULT_BASE_PATH } from "./test-constants";

/**
 * Mount/unmount contract for the React binding.
 *
 * Replaces SessionRecorder.single/final/retry.test.tsx, which characterised the
 * previous standalone React implementation: they asserted on console.log text
 * ("attempt 1/3") and, more importantly, required a re-render with unchanged
 * props to fire another `/session-replay/init` request. Driving network calls
 * from renders is the behaviour this component deliberately dropped when it
 * became a wrapper over @temps-sdk/analytics-core, so those assertions now
 * describe a bug rather than a requirement. The concerns they covered — the
 * retry ceiling and remount behaviour — are asserted here against the current
 * contract.
 */

function initCalls(fetchSpy: ReturnType<typeof vi.spyOn>): unknown[] {
  return fetchSpy.mock.calls.filter((call: unknown[]) =>
    String(call[0] ?? "").includes("/session-replay/init"),
  );
}

describe("SessionRecorder lifecycle", () => {
  let fetchSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.clearAllMocks();
    fetchSpy = vi.spyOn(global, "fetch");
    Object.defineProperty(global, "crypto", {
      value: { randomUUID: vi.fn(() => `test-session-${Math.random()}`) },
      writable: true,
    });
    Object.defineProperty(window, "location", {
      value: {
        hostname: "example.com",
        pathname: "/test",
        search: "",
        href: "https://example.com/test",
        protocol: "https:",
      },
      writable: true,
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("initializes exactly once and does not re-initialize on re-render", async () => {
    fetchSpy.mockResolvedValue(new Response(null, { status: 201 }));

    const { rerender } = render(
      <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
    );

    await waitFor(() => expect(initCalls(fetchSpy).length).toBe(1));

    for (let i = 0; i < 5; i++) {
      rerender(
        <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
      );
      await new Promise((resolve) => setTimeout(resolve, 20));
    }

    // Renders are not network events: the recorder built on mount is reused.
    expect(initCalls(fetchSpy).length).toBe(1);
  });

  it("never exceeds the init retry ceiling when the endpoint keeps failing", async () => {
    fetchSpy.mockRejectedValue(new Error("network down"));

    const { rerender } = render(
      <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
    );

    await waitFor(() => expect(initCalls(fetchSpy).length).toBeGreaterThan(0));

    for (let i = 0; i < 6; i++) {
      rerender(
        <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
      );
      await new Promise((resolve) => setTimeout(resolve, 20));
    }

    expect(initCalls(fetchSpy).length).toBeLessThanOrEqual(3);
  });

  it("does not touch the network at all while disabled", async () => {
    fetchSpy.mockResolvedValue(new Response(null, { status: 201 }));

    render(
      <SessionRecorder enabled={false} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
    );
    await new Promise((resolve) => setTimeout(resolve, 50));

    expect(initCalls(fetchSpy).length).toBe(0);
  });

  it("starts a fresh session after unmount and remount", async () => {
    fetchSpy.mockResolvedValue(new Response(null, { status: 201 }));

    const first = render(
      <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
    );
    await waitFor(() => expect(initCalls(fetchSpy).length).toBe(1));
    first.unmount();

    render(<SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />);
    await waitFor(() => expect(initCalls(fetchSpy).length).toBe(2));
  });
});
