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
    // The response omitted `other` (removed, archived or server-only), so
    // its bootstrap value is retired.
    expect(flags.get("other", "fallback")).toBe("fallback");
  });

  it("keeps bootstrap values when the request fails", async () => {
    const flags = new FeatureFlags({ fetch: served([], 503), bootstrap: { other: "x" }, onError: () => {} });
    await flags.ready();
    expect(flags.status).toBe("error");
    expect(flags.get("other", "fallback")).toBe("x");
  });

  it("never serves a value of the wrong type, and records no exposure for it", async () => {
    const onExposure = vi.fn();
    const flags = new FeatureFlags({ fetch: served([{ ...checkout, value: "yes" }]), onExposure });
    await flags.ready();
    expect(flags.get("checkout.v2", false)).toBe(false);
    expect(onExposure).not.toHaveBeenCalled();
  });

  it("peeks without fetching or recording an exposure", async () => {
    const onExposure = vi.fn();
    const fetch = served([checkout]);
    const flags = new FeatureFlags({ fetch, onExposure });
    expect(flags.peek("checkout.v2", false)).toBe(false);
    expect(fetch).not.toHaveBeenCalled();
    flags.get("checkout.v2", false);
    await flags.ready();
    expect(flags.peek("checkout.v2", false)).toBe(true);
    expect(flags.peekVariant("checkout.v2")).toBe("test");
    expect(onExposure).not.toHaveBeenCalled();
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
    const request = flags.ready();
    await Promise.resolve();
    expect(flags.status).toBe("loading");
    expect(listener).toHaveBeenCalledOnce();
    await request;
    expect(listener).toHaveBeenCalledTimes(2);
    expect(flags.version).toBe(before + 2);
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

  it("replaces the context, re-fetching only once flags were requested", async () => {
    const fetch = served([checkout]);
    const flags = new FeatureFlags({ fetch, key: "anon", attributes: { plan: "free", beta: true } });
    flags.setContext({ key: "user-1", attributes: { plan: "pro" } });
    expect(fetch).not.toHaveBeenCalled();

    await flags.ready();
    flags.setContext({ key: "user-2", attributes: { plan: "team" } });
    await flags.ready();
    expect(fetch).toHaveBeenCalledTimes(2);
    const bodies = fetch.mock.calls.map((call) => JSON.parse((call as unknown as [string, RequestInit])[1].body as string));
    expect(bodies[0]).toEqual({ key: "user-1", attributes: { plan: "pro" } });
    expect(bodies[1]).toEqual({ key: "user-2", attributes: { plan: "team" } });
  });

  it("drops the previous subject's values when the context changes", async () => {
    let status = 200;
    const fetch = vi.fn(async () =>
      new Response(JSON.stringify({ flags: [checkout] }), { status, headers: { "Content-Type": "application/json" } })
    );
    const flags = new FeatureFlags({
      fetch,
      key: "user-1",
      bootstrap: { "checkout.v2": true },
      onError: () => undefined,
    });
    await flags.ready();
    expect(flags.get("checkout.v2", false)).toBe(true);

    // Sign-out: the next request fails, and nothing of user-1's remains.
    status = 429;
    flags.setContext({});
    expect(flags.peek("checkout.v2", false)).toBe(false);
    await flags.ready();
    expect(flags.get("checkout.v2", false)).toBe(false);

    // A refresh with the same context keeps the last good values.
    status = 200;
    await flags.reload();
    expect(flags.get("checkout.v2", false)).toBe(true);
    status = 503;
    await flags.reload({ attributes: {} });
    expect(flags.get("checkout.v2", false)).toBe(true);
  });

  it("treats attributes in another order as the same context", async () => {
    const fetch = served([checkout]);
    const flags = new FeatureFlags({ fetch, attributes: { a: 1, b: 2 } });
    await flags.ready();
    flags.setContext({ attributes: { b: 2, a: 1 } });
    expect(flags.peek("checkout.v2", false)).toBe(true);
  });

  it("can be disabled and re-enabled after construction", async () => {
    const fetch = served([checkout]);
    const flags = new FeatureFlags({ fetch, disabled: true });
    flags.get("checkout.v2", false);
    expect(fetch).not.toHaveBeenCalled();

    flags.setDisabled(false);
    expect(flags.get("checkout.v2", false)).toBe(false);
    await flags.ready();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(flags.get("checkout.v2", false)).toBe(true);

    flags.setDisabled(true);
    // Disabled means bootstrap or fallback only, not what was fetched.
    expect(flags.get("checkout.v2", false)).toBe(false);
    expect(flags.peek("checkout.v2", false)).toBe(false);
    flags.setContext({ key: "user-1" });
    flags.get("checkout.v2", false);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("fails a request that stalls, keeping the bootstrap values", async () => {
    vi.useFakeTimers();
    try {
      const onError = vi.fn();
      // Headers arrive, then the body never finishes.
      const fetch = vi.fn(async () => ({ ok: true, status: 200, json: () => new Promise(() => undefined) }) as unknown as Response);
      const flags = new FeatureFlags({ fetch, onError, bootstrap: { "checkout.v2": true }, timeoutMs: 1_000 });
      const ready = flags.ready();
      expect(flags.status).toBe("loading");
      await vi.advanceTimersByTimeAsync(1_000);
      await ready;
      expect(flags.status).toBe("error");
      expect(onError).toHaveBeenCalledOnce();
      expect(String(onError.mock.calls[0]?.[0])).toContain("did not answer within 1s");
      expect(flags.get("checkout.v2", false)).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("never fetches when disabled", async () => {
    const fetch = served([checkout]);
    const flags = new FeatureFlags({ fetch, disabled: true, bootstrap: { "checkout.v2": true } });
    expect(flags.get("checkout.v2", false)).toBe(true);
    await flags.ready();
    expect(fetch).not.toHaveBeenCalled();
  });
});
