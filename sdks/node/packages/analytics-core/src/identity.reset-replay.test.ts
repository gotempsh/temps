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
  let resetResponse: () => Promise<Response>;

  beforeEach(() => {
    localStorage.clear();
    resetResponse = async () => new Response(null, { status: 204 });
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation((url: string) =>
        String(url).endsWith("/reset")
          ? resetResponse()
          : Promise.resolve(new Response(null, { status: 204 }))
      )
    );
  });

  afterEach(() => {
    for (const instance of instances.splice(0)) instance.destroy();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("ends the replay at once and starts a new one only after the server confirmed", async () => {
    const end = vi.spyOn(SessionRecorder.prototype, "endForReset");
    const resume = vi.spyOn(SessionRecorder.prototype, "resumeAfterReset");
    let confirm: () => void = () => undefined;
    resetResponse = () =>
      new Promise<Response>((resolve) => {
        confirm = () => resolve(new Response(null, { status: 204 }));
      });
    const recording = createAnalytics({ ...quiet, enableSessionRecording: true });
    instances.push(recording);

    const pending = recording.reset();
    expect(end).toHaveBeenCalledTimes(1);
    expect(resume).not.toHaveBeenCalled();
    confirm();
    await pending;
    expect(resume).toHaveBeenCalledTimes(1);
  });

  it("starts no new replay when the server did not confirm the reset", async () => {
    const resume = vi.spyOn(SessionRecorder.prototype, "resumeAfterReset");
    resetResponse = async () => new Response(null, { status: 503 });
    const recording = createAnalytics({ ...quiet, enableSessionRecording: true });
    instances.push(recording);
    await expect(recording.reset()).rejects.toThrow(/503/);
    expect(resume).not.toHaveBeenCalled();
  });

  it("follows a reset from another instance, and stops listening once destroyed", async () => {
    const end = vi.spyOn(SessionRecorder.prototype, "endForReset");
    const recording = createAnalytics({ ...quiet, enableSessionRecording: true });
    const other = createAnalytics(quiet);
    instances.push(other);
    await other.reset();
    expect(end).toHaveBeenCalledTimes(1);
    recording.destroy();
    await other.reset();
    expect(end).toHaveBeenCalledTimes(1);
  });
});

describe("SessionRecorder across a reset", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("resumes only a recording endForReset stopped", () => {
    const recorder = new SessionRecorder({ enabled: false });
    const start = vi.spyOn(recorder, "start").mockImplementation(() => undefined);
    recorder.endForReset();
    recorder.resumeAfterReset();
    expect(start).not.toHaveBeenCalled();

    (recorder as unknown as { enabled: boolean }).enabled = true;
    recorder.endForReset();
    recorder.resumeAfterReset();
    recorder.resumeAfterReset();
    expect(start).toHaveBeenCalledTimes(1);
  });

  it("never sends an undelivered batch of the ended replay under the next one", async () => {
    // The final upload of the ended replay fails: no beacon, and fetch rejects.
    vi.stubGlobal("navigator", { userAgent: "test", language: "en", sendBeacon: () => false });
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const recorder = new SessionRecorder({ enabled: false });
    const internal = recorder as unknown as {
      enabled: boolean;
      sessionInitialized: boolean;
      sessionId: string;
      pending: unknown[];
      claimBatch(): { events: unknown[] } | null;
    };
    const oldEvent = { type: 3, timestamp: 1, data: { source: 0 } };
    internal.enabled = true;
    internal.sessionInitialized = true;
    internal.sessionId = "replay-of-the-signed-out-visitor";
    internal.pending = [oldEvent];

    recorder.endForReset();
    await new Promise((resolve) => setTimeout(resolve, 0));

    // The next visitor's replay.
    const newEvent = { type: 3, timestamp: 2, data: { source: 1 } };
    internal.sessionInitialized = true;
    internal.sessionId = "replay-of-the-next-visitor";
    internal.pending = [newEvent];
    expect(internal.claimBatch()?.events).toEqual([newEvent]);
  });
});
