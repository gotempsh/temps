// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getOrCreateSessionId, getOrCreateVisitorId } from "./identity";

describe("getOrCreateVisitorId", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("generates and persists a visitor id on first call", () => {
    expect(localStorage.getItem("temps_visitor_id")).toBeNull();
    const id = getOrCreateVisitorId();
    expect(id).toBeTruthy();
    expect(localStorage.getItem("temps_visitor_id")).toBe(id);
  });

  it("returns the same id on every subsequent call", () => {
    const first = getOrCreateVisitorId();
    const second = getOrCreateVisitorId();
    expect(second).toBe(first);
  });

  it("reuses an id already present in localStorage (e.g. from an older SDK version)", () => {
    localStorage.setItem("temps_visitor_id", "visitor_legacy_123");
    expect(getOrCreateVisitorId()).toBe("visitor_legacy_123");
  });
});

describe("getOrCreateSessionId", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("generates a session id on first call", () => {
    const id = getOrCreateSessionId();
    expect(id).toBeTruthy();
    expect(localStorage.getItem("temps_session_id")).toBe(id);
  });

  it("keeps the same session id across calls within the inactivity window", () => {
    const first = getOrCreateSessionId();
    vi.advanceTimersByTime(10 * 60 * 1000); // 10 minutes
    const second = getOrCreateSessionId();
    expect(second).toBe(first);
  });

  it("mints a new session id once the 30-minute inactivity window elapses", () => {
    const first = getOrCreateSessionId();
    vi.advanceTimersByTime(31 * 60 * 1000); // 31 minutes
    const second = getOrCreateSessionId();
    expect(second).not.toBe(first);
  });

  it("does not roll over just under the 30-minute boundary", () => {
    const first = getOrCreateSessionId();
    vi.advanceTimersByTime(29 * 60 * 1000);
    const second = getOrCreateSessionId();
    expect(second).toBe(first);
  });
});
