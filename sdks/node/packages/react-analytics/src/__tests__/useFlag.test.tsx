// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider } from "../Provider";
import { useFeatureFlags, useFlag, useFlagVariant } from "../useFlag";
import { DEFAULT_BASE_PATH } from "./test-constants";

function serveFlags() {
  return vi.spyOn(global, "fetch").mockImplementation(async (input) => {
    if (String(input).endsWith("/flags")) {
      return new Response(
        JSON.stringify({
          flags: [
            {
              key: "checkout.v2",
              value: true,
              variant: "test",
              reason: { kind: "PERCENTAGE_ROLLOUT", index: 0 },
              record_exposure: false,
            },
          ],
        }),
        { status: 200 }
      );
    }
    return new Response("ok", { status: 200 });
  });
}

const wrapper = ({ children }: { children: React.ReactNode }) => (
  <TempsAnalyticsProvider ignoreLocalhost={false} flags={{ attributes: { plan: "pro" } }}>
    {children}
  </TempsAnalyticsProvider>
);

describe("useFlag", () => {
  it("returns the fallback, then re-renders with the served value", async () => {
    const fetchSpy = serveFlags();
    const { result } = renderHook(
      () => ({ value: useFlag("checkout.v2", false), variant: useFlagVariant("checkout.v2") }),
      { wrapper }
    );

    expect(result.current.value).toBe(false);
    await waitFor(() => expect(result.current.value).toBe(true));
    expect(result.current.variant).toBe("test");

    const flagCalls = fetchSpy.mock.calls.filter(([url]) => String(url) === `${DEFAULT_BASE_PATH}/flags`);
    expect(flagCalls).toHaveLength(1);
    expect(JSON.parse((flagCalls[0][1] as RequestInit).body as string).attributes).toEqual({ plan: "pro" });
  });

  it("keeps the fallback for a flag the server did not serve", async () => {
    serveFlags();
    const { result } = renderHook(() => useFeatureFlags(), { wrapper });
    // Reading the status alone fetches nothing; the first read does.
    expect(result.current.status).toBe("idle");
    expect(result.current.flags.get("server.only", "fallback")).toBe("fallback");
    await waitFor(() => expect(result.current.status).toBe("ready"));
    expect(result.current.flags.get("server.only", "fallback")).toBe("fallback");
    expect(result.current.flags.get("checkout.v2", false)).toBe(true);
  });

  it("re-evaluates with a new context on reload", async () => {
    const fetchSpy = serveFlags();
    const { result } = renderHook(() => useFeatureFlags(), { wrapper });
    await result.current.reload({ key: "user-42" });

    const last = fetchSpy.mock.calls.filter(([url]) => String(url).endsWith("/flags")).at(-1);
    const body = JSON.parse((last![1] as RequestInit).body as string);
    expect(body.key).toBe("user-42");
    expect(body.attributes).toEqual({ plan: "pro" });
  });
});
