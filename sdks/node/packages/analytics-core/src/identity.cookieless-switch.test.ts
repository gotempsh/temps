// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Its own file: the switch to cookieless is one-way for the module registry,
// and this test needs a persistent instance to exist first.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAnalytics } from "./Analytics";
import { SessionRecorder } from "./SessionRecorder";
import type { AnalyticsApi } from "./types";

describe("switching the page to cookieless (ADR 109 D3)", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  const instances: AnalyticsApi[] = [];

  beforeEach(() => {
    localStorage.clear();
    fetchMock = vi.fn().mockResolvedValue({ status: 204, ok: true, json: async () => ({}) });
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    for (const instance of instances.splice(0)) instance.destroy();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("ends a replay an earlier instance started and drops its unsent events", () => {
    const discard = vi.spyOn(SessionRecorder.prototype, "discard");
    const quiet = {
      ignoreLocalhost: false,
      autoTrackPageviews: false,
      autoTrackPageLeave: false,
      autoTrackEngagement: false,
      autoTrackSpeedAnalytics: false,
    };
    const recording = createAnalytics({ ...quiet, enableSessionRecording: true });
    instances.push(recording);
    expect(discard).not.toHaveBeenCalled();

    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    instances.push(createAnalytics({ ...quiet, cookieless: true }));
    expect(discard).toHaveBeenCalledTimes(1);

    // Nothing restarts it: the page stays cookieless.
    const callsAfterSwitch = fetchMock.mock.calls.length;
    recording.enableSessionRecording();
    const replayCalls = fetchMock.mock.calls
      .slice(callsAfterSwitch)
      .filter(([url]) => String(url).includes("session-replay"));
    expect(replayCalls).toHaveLength(0);
    expect(discard).toHaveBeenCalledTimes(1);
  });
});
