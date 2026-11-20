// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it, vi } from "vitest";
import { FeatureFlags, type FlagEvaluation } from "./FeatureFlags";

function served(flags: FlagEvaluation[], status = 200) {
  return vi.fn(async () =>
    new Response(JSON.stringify({ flags }), {
      status,
      headers: { "Content-Type": "application/json" },
    })
  );
}

const checkout: FlagEvaluation = {
  key: "checkout.v2",
  value: true,
  variant: "test",
  reason: { kind: "PERCENTAGE_ROLLOUT", index: 0 },
  record_exposure: true,
};

describe("FeatureFlags", () => {
  it("fetches nothing until a flag is first read", async () => {
    const fetch = served([checkout]);
    const flags = new FeatureFlags({ fetch });
    expect(fetch).not.toHaveBeenCalled();
    expect(flags.status).toBe("idle");

    expect(flags.get("checkout.v2", false)).toBe(false);
    expect(fetch).toHaveBeenCalledTimes(1);
    await flags.ready();
    expect(flags.get("checkout.v2", false)).toBe(true);
    expect(flags.getVariant("checkout.v2")).toBe("test");
  });

  it("posts the context to the same-origin endpoint", async () => {
    const fetch = served([]);
    const flags = new FeatureFlags({ fetch, basePath: "/api/_temps", key: "user-1", attributes: { plan: "pro" } });
    await flags.ready();

    const [url, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/_temps/flags");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ key: "user-1", attributes: { plan: "pro" } });
  });

  it("serves bootstrap values until the response arrives", async () => {
    const flags = new FeatureFlags({ fetch: served([checkout]), bootstrap: { "checkout.v2": true, other: "x" } });
    expect(flags.get("checkout.v2", false)).toBe(true);
    expect(flags.get("other", "fallback")).toBe("x");
    await flags.ready();
    expect(flags.get("checkout.v2", false)).toBe(true);
  });

  it("never serves a value of the wrong type", async () => {
    const flags = new FeatureFlags({ fetch: served([{ ...checkout, value: "yes" }]) });
    await flags.ready();
    expect(flags.get("checkout.v2", false)).toBe(false);
  });

  it("keeps fallbacks and reports the error when the request fails", async () => {
    const onError = vi.fn();
    const flags = new FeatureFlags({ fetch: served([], 503), onError });
    await flags.ready();
    expect(flags.status).toBe("error");
    expect(flags.get("checkout.v2", "fallback")).toBe("fallback");
    expect(onError).toHaveBeenCalledOnce();
  });

  it("records an exposure once per served variant, only when the server asks", async () => {
    const onExposure = vi.fn();
    const flags = new FeatureFlags({
      fetch: served([checkout, { ...checkout, key: "quiet", record_exposure: false }]),
      onExposure,
    });
    await flags.ready();

    flags.get("checkout.v2", false);
    flags.get("checkout.v2", false);
    flags.getVariant("checkout.v2");
    flags.get("quiet", false);
    flags.getEvaluation("checkout.v2");

    expect(onExposure).toHaveBeenCalledTimes(1);
    expect(onExposure).toHaveBeenCalledWith({
      flag_key: "checkout.v2",
      variant: "test",
      reason: "PERCENTAGE_ROLLOUT",
    });
  });

  it("notifies subscribers and bumps the version when flags arrive", async () => {
    const flags = new FeatureFlags({ fetch: served([checkout]) });
    const listener = vi.fn();
    flags.subscribe(listener);
    const before = flags.version;
    await flags.ready();
    expect(listener).toHaveBeenCalledOnce();
    expect(flags.version).toBe(before + 1);
  });

  it("lets a reload win over an older request still in flight", async () => {
    let releaseFirst: (response: Response) => void = () => {};
    const fetch = vi
      .fn()
      .mockImplementationOnce(() => new Promise<Response>((resolve) => (releaseFirst = resolve)))
      .mockImplementationOnce(async () => new Response(JSON.stringify({ flags: [checkout] })));
    const flags = new FeatureFlags({ fetch });

    flags.get("checkout.v2", false);
    await flags.reload({ key: "user-1" });
    releaseFirst(new Response(JSON.stringify({ flags: [] })));
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(flags.get("checkout.v2", false)).toBe(true);
    expect(JSON.parse((fetch.mock.calls[1][1] as RequestInit).body as string).key).toBe("user-1");
  });

  it("never fetches when disabled", async () => {
    const fetch = served([checkout]);
    const flags = new FeatureFlags({ fetch, disabled: true, bootstrap: { "checkout.v2": true } });
    expect(flags.get("checkout.v2", false)).toBe(true);
    await flags.ready();
    expect(fetch).not.toHaveBeenCalled();
  });
});
