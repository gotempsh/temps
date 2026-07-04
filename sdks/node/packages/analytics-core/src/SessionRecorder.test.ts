import { describe, it, expect, afterEach } from "vitest";
import { SessionRecorder, type SessionRecorderOptions } from "./SessionRecorder";
import { DEFAULT_EXCLUDED_PATHS } from "./constants";

/**
 * `shouldRecord()` is private, but it is the single source of truth for
 * whether a given pathname is recorded. We flip `enabled` on directly
 * (bypassing `start()`, which would spin up rrweb/network calls) so these
 * tests exercise the real exclusion logic without needing to mock rrweb or
 * fetch.
 */
function shouldRecordAt(path: string, options: SessionRecorderOptions = {}): boolean {
  const recorder = new SessionRecorder({ ...options, enabled: false });
  const internal = recorder as unknown as { enabled: boolean; shouldRecord(): boolean };
  internal.enabled = true;
  window.history.pushState({}, "", path);
  return internal.shouldRecord();
}

describe("SessionRecorder excludedPaths defaults", () => {
  afterEach(() => {
    window.history.pushState({}, "", "/");
  });

  it("ships the documented list of default excluded paths", () => {
    expect(DEFAULT_EXCLUDED_PATHS).toEqual([
      "/login",
      "/log-in",
      "/signin",
      "/sign-in",
      "/logout",
      "/log-out",
      "/signup",
      "/sign-up",
      "/register",
      "/checkout*",
      "/payment*",
      "/billing*",
      "/reset-password*",
      "/forgot-password*",
      "/mfa*",
      "/2fa*",
      "/verify*",
    ]);
  });

  it("excludes sensitive routes out of the box with zero configuration", () => {
    const sensitivePaths = [
      "/login",
      "/log-in",
      "/signin",
      "/sign-in",
      "/logout",
      "/signup",
      "/sign-up",
      "/register",
      "/checkout",
      "/checkout/step-2",
      "/payment/confirm",
      "/billing",
      "/billing/invoices",
      "/reset-password",
      "/forgot-password",
      "/mfa/verify",
      "/2fa",
      "/verify-email",
    ];

    for (const path of sensitivePaths) {
      expect(shouldRecordAt(path)).toBe(false);
    }
  });

  it("records a non-sensitive path normally by default", () => {
    expect(shouldRecordAt("/dashboard")).toBe(true);
    expect(shouldRecordAt("/")).toBe(true);
    expect(shouldRecordAt("/blog/hello-world")).toBe(true);
  });

  it("merges user-supplied excludedPaths additively instead of replacing the defaults", () => {
    const options: SessionRecorderOptions = { excludedPaths: ["/internal-tool"] };

    // The user-supplied path is now excluded too...
    expect(shouldRecordAt("/internal-tool", options)).toBe(false);
    // ...but the built-in defaults are still active alongside it.
    expect(shouldRecordAt("/login", options)).toBe(false);
    expect(shouldRecordAt("/checkout/step-1", options)).toBe(false);
    // Unrelated paths are still recorded.
    expect(shouldRecordAt("/dashboard", options)).toBe(true);
  });

  it("supports opting out of the built-in defaults entirely via useDefaultExcludedPaths: false", () => {
    const options: SessionRecorderOptions = {
      excludedPaths: ["/internal-tool"],
      useDefaultExcludedPaths: false,
    };

    // Only the user-supplied path is excluded now.
    expect(shouldRecordAt("/internal-tool", options)).toBe(false);
    // The default sensitive paths are no longer excluded.
    expect(shouldRecordAt("/login", options)).toBe(true);
    expect(shouldRecordAt("/checkout", options)).toBe(true);
  });

  it("records everything when useDefaultExcludedPaths is false and no excludedPaths are given", () => {
    const options: SessionRecorderOptions = { useDefaultExcludedPaths: false };

    expect(shouldRecordAt("/login", options)).toBe(true);
    expect(shouldRecordAt("/checkout", options)).toBe(true);
    expect(shouldRecordAt("/dashboard", options)).toBe(true);
  });
});
