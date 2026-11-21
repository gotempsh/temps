// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAnalytics } from "./Analytics";
import { SessionRecorder } from "./SessionRecorder";
import { isIdentityResetPending, notifyIdentityReset } from "./identity";
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
    // A test that ends with a reset the server refused leaves it pending.
    notifyIdentityReset("confirmed");
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

  it("keeps recording off when the app disabled it while the reset was pending", async () => {
    const start = vi.spyOn(SessionRecorder.prototype, "start");
    let confirm: () => void = () => undefined;
    resetResponse = () =>
      new Promise<Response>((resolve) => {
        confirm = () => resolve(new Response(null, { status: 204 }));
      });
    const recording = createAnalytics({ ...quiet, enableSessionRecording: true });
    instances.push(recording);
    const startsBefore = start.mock.calls.length;

    const pending = recording.reset();
    recording.disableSessionRecording();
    confirm();
    await pending;
    expect(start.mock.calls.length).toBe(startsBefore);
  });

  it("starts a recorder created while a reset is pending only once the server confirmed it", async () => {
    const startRecording = vi
      .spyOn(SessionRecorder.prototype as unknown as { startRecording(): Promise<void> }, "startRecording")
      .mockResolvedValue(undefined);
    let confirm: () => void = () => undefined;
    resetResponse = () =>
      new Promise<Response>((resolve) => {
        confirm = () => resolve(new Response(null, { status: 204 }));
      });
    const resetter = createAnalytics(quiet);
    instances.push(resetter);

    const pending = resetter.reset();
    expect(isIdentityResetPending()).toBe(true);
    const lateRecorder = createAnalytics({ ...quiet, enableSessionRecording: true });
    instances.push(lateRecorder);
    const lateRuntime = createAnalytics(quiet);
    instances.push(lateRuntime);
    lateRuntime.enableSessionRecording();
    expect(startRecording).not.toHaveBeenCalled();

    confirm();
    await pending;
    expect(isIdentityResetPending()).toBe(false);
    expect(startRecording).toHaveBeenCalledTimes(2);
  });

  it("keeps a recorder created after a refused reset off until a reset succeeds", async () => {
    const startRecording = vi
      .spyOn(SessionRecorder.prototype as unknown as { startRecording(): Promise<void> }, "startRecording")
      .mockResolvedValue(undefined);
    resetResponse = async () => new Response(null, { status: 503 });
    const resetter = createAnalytics(quiet);
    instances.push(resetter);
    await expect(resetter.reset()).rejects.toThrow(/503/);

    instances.push(createAnalytics({ ...quiet, enableSessionRecording: true }));
    expect(startRecording).not.toHaveBeenCalled();

    resetResponse = async () => new Response(null, { status: 204 });
    await resetter.reset();
    expect(startRecording).toHaveBeenCalledTimes(1);
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

  it("an explicit stop or destroy cancels the replay a pending reset would start", () => {
    for (const end of ["stop", "destroy"] as const) {
      const recorder = new SessionRecorder({ enabled: false });
      const start = vi.spyOn(recorder, "start").mockImplementation(() => undefined);
      (recorder as unknown as { enabled: boolean }).enabled = true;
      recorder.endForReset();
      recorder[end]();
      recorder.resumeAfterReset();
      expect(start).not.toHaveBeenCalled();
    }
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
