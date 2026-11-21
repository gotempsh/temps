// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAnalytics } from "./Analytics";
import { SessionRecorder } from "./SessionRecorder";
import type { AnalyticsApi } from "./types";

describe("reset() and session replay (ADR 109 D4)", () => {
  const instances: AnalyticsApi[] = [];
  const quiet = {
    ignoreLocalhost: false,
    autoTrackPageviews: false,
    autoTrackPageLeave: false,
    autoTrackEngagement: false,
    autoTrackSpeedAnalytics: false,
  };

  beforeEach(() => {
    localStorage.clear();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ status: 204, ok: true, json: async () => ({}) }));
  });

  afterEach(() => {
    for (const instance of instances.splice(0)) instance.destroy();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("ends the replay in progress and starts a new one, from any instance", async () => {
    const restart = vi.spyOn(SessionRecorder.prototype, "restart");
    const recording = createAnalytics({ ...quiet, enableSessionRecording: true });
    instances.push(recording);
    const other = createAnalytics(quiet);
    instances.push(other);
    expect(restart).not.toHaveBeenCalled();

    // Signing out on the same page, through either tracker.
    await recording.reset();
    expect(restart).toHaveBeenCalledTimes(1);
    await other.reset();
    expect(restart).toHaveBeenCalledTimes(2);
  });

  it("stops listening once the instance is destroyed", async () => {
    const restart = vi.spyOn(SessionRecorder.prototype, "restart");
    const recording = createAnalytics({ ...quiet, enableSessionRecording: true });
    recording.destroy();
    const other = createAnalytics(quiet);
    instances.push(other);
    await other.reset();
    expect(restart).not.toHaveBeenCalled();
  });
});

describe("SessionRecorder.restart", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("stops the replay and starts a new one when recording was on", () => {
    const recorder = new SessionRecorder({ enabled: false });
    const stop = vi.spyOn(recorder, "stop");
    const start = vi.spyOn(recorder, "start").mockImplementation(() => undefined);

    recorder.restart();
    expect(stop).toHaveBeenCalledTimes(1);
    expect(start).not.toHaveBeenCalled();

    (recorder as unknown as { enabled: boolean }).enabled = true;
    recorder.restart();
    expect(stop).toHaveBeenCalledTimes(2);
    expect(start).toHaveBeenCalledTimes(1);
  });
});
