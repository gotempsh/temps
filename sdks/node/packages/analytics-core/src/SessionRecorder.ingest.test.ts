// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

/**
 * Regression tests for the ingest path: batch immutability across retries, the
 * double-recorder race, idle/visibility pausing, and the buffer bound.
 *
 * rrweb is mocked so a test can drive `emit` directly and count how many
 * recorders got attached; `pack` is mocked to a passthrough so assertions can
 * read the events actually transmitted.
 */

/** Every `record()` call made by the recorder, with its emit callback. */
const recordCalls: Array<{ emit: (event: unknown) => void; stop: ReturnType<typeof vi.fn> }> = [];

vi.mock("rrweb", () => ({
  record: (options: { emit: (event: unknown) => void }) => {
    const stop = vi.fn();
    recordCalls.push({ emit: options.emit, stop });
    return stop;
  },
}));

vi.mock("@rrweb/packer", () => ({
  pack: (events: unknown) => JSON.stringify(events),
}));

import { SessionRecorder } from "./SessionRecorder";

interface SentRequest {
  sessionId: string;
  batchId: string;
  events: unknown[];
}

/** Decode a captured request body back into the events it carried. */
function decodeBody(body: string): SentRequest {
  const parsed = JSON.parse(body) as { sessionId: string; batchId: string; events: string };
  return { ...parsed, events: JSON.parse(atob(parsed.events)) as unknown[] };
}

function mkEvent(id: number): { type: number; timestamp: number; id: number } {
  return { type: 3, timestamp: 1000 + id, id };
}

/**
 * Recorders created by a test. They must be destroyed between tests: a paused
 * recorder deliberately keeps its activity listeners attached (that is how it
 * learns to resume), so a leaked one from an earlier test would wake up and
 * attach another rrweb recorder the moment a later test dispatches input.
 */
const liveRecorders: SessionRecorder[] = [];

function newRecorder(options: Record<string, unknown> = {}): SessionRecorder {
  const recorder = new SessionRecorder(options);
  liveRecorders.push(recorder);
  return recorder;
}

/** A recorder that has completed session init and attached one rrweb recorder. */
async function startedRecorder(
  options: Record<string, unknown> = {},
): Promise<{ recorder: SessionRecorder; emit: (event: unknown) => void }> {
  const recorder = newRecorder({ enabled: true, ...options });
  await vi.waitFor(() => expect(recordCalls.length).toBe(1));
  return { recorder, emit: recordCalls[0].emit };
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  recordCalls.length = 0;
  liveRecorders.length = 0;
  vi.useFakeTimers({ shouldAdvanceTime: true });
  fetchMock = vi.fn().mockResolvedValue({ status: 201, ok: true });
  vi.stubGlobal("fetch", fetchMock);
  // sendBeacon absent so unload flushes go through fetch and stay observable.
  vi.stubGlobal("navigator", { userAgent: "test", language: "en", sendBeacon: undefined });
});

afterEach(() => {
  for (const recorder of liveRecorders) recorder.destroy();
  liveRecorders.length = 0;
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  localStorage.clear();
});

/** Requests to the events endpoint, in order. */
function eventPosts(): SentRequest[] {
  return fetchMock.mock.calls
    .filter(([url]) => String(url).endsWith("/session-replay/events"))
    .map(([, init]) => decodeBody((init as { body: string }).body));
}

describe("batch immutability across retries", () => {
  it("resends the identical batch under the same batchId after a failure", async () => {
    // Short flushInterval so the retry tick lands inside the test.
    const { emit } = await startedRecorder({ batchSize: 2, flushInterval: 500 });

    // First flush fails.
    fetchMock.mockResolvedValueOnce({ status: 500, ok: false });
    emit(mkEvent(1));
    emit(mkEvent(2));
    await vi.waitFor(() => expect(eventPosts()).toHaveLength(1));

    // Events that arrive while the failed batch is held must not join it.
    emit(mkEvent(3));
    emit(mkEvent(4));

    // Backoff is 2s after one failure; advance past it so a flush tick retries.
    await vi.advanceTimersByTimeAsync(4000);
    await vi.waitFor(() => expect(eventPosts().length).toBeGreaterThanOrEqual(2));

    const [first, second] = eventPosts();
    expect(second.batchId).toBe(first.batchId);
    expect(second.events).toEqual(first.events);
    expect(second.events).toHaveLength(2);
  });

  it("does not drop events emitted while a request is in flight", async () => {
    let releaseFirst: (value: { status: number; ok: boolean }) => void = () => {};
    fetchMock.mockImplementationOnce(() => Promise.resolve({ status: 201, ok: true }));

    const { emit } = await startedRecorder({ batchSize: 2 });

    fetchMock.mockImplementationOnce(
      () => new Promise((resolve) => { releaseFirst = resolve; }),
    );
    emit(mkEvent(1));
    emit(mkEvent(2));
    await vi.waitFor(() => expect(eventPosts()).toHaveLength(1));

    // Emitted mid-flight: previously wiped by an unconditional `events = []`.
    emit(mkEvent(3));
    emit(mkEvent(4));
    releaseFirst({ status: 200, ok: true });

    await vi.advanceTimersByTimeAsync(11000);
    await vi.waitFor(() => expect(eventPosts().length).toBeGreaterThanOrEqual(2));

    const delivered = eventPosts().flatMap((r) => r.events as Array<{ id: number }>);
    expect(delivered.map((e) => e.id).sort()).toEqual([1, 2, 3, 4]);
  });

  it("gives each distinct batch its own batchId", async () => {
    const { emit } = await startedRecorder({ batchSize: 2 });

    emit(mkEvent(1));
    emit(mkEvent(2));
    await vi.waitFor(() => expect(eventPosts()).toHaveLength(1));
    emit(mkEvent(3));
    emit(mkEvent(4));
    await vi.waitFor(() => expect(eventPosts()).toHaveLength(2));

    const [first, second] = eventPosts();
    expect(second.batchId).not.toBe(first.batchId);
  });
});

describe("double-recorder race", () => {
  it("attaches exactly one rrweb recorder when startRecording races itself", async () => {
    const recorder = newRecorder({ enabled: false });
    const internal = recorder as unknown as { enabled: boolean; startRecording(): Promise<void> };
    internal.enabled = true;

    // Both calls enter while session init is still in flight — the shape a
    // router produces by touching replaceState during startup.
    await Promise.all([internal.startRecording(), internal.startRecording()]);

    expect(recordCalls).toHaveLength(1);
  });

  it("issues a single session init for racing starts", async () => {
    const recorder = newRecorder({ enabled: false });
    const internal = recorder as unknown as { enabled: boolean; startRecording(): Promise<void> };
    internal.enabled = true;

    await Promise.all([internal.startRecording(), internal.startRecording()]);

    const inits = fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/session-replay/init"));
    expect(inits).toHaveLength(1);
  });
});

describe("idle and visibility pausing", () => {
  it("detaches rrweb once idleTimeout elapses with no interaction", async () => {
    const { recorder } = await startedRecorder({ idleTimeout: 1000 });
    expect(recorder.isPaused()).toBe(false);

    await vi.advanceTimersByTimeAsync(2000);

    expect(recorder.isPaused()).toBe(true);
    expect(recordCalls[0].stop).toHaveBeenCalled();
  });

  it("resumes on the next interaction, reusing the same session", async () => {
    const { recorder } = await startedRecorder({ idleTimeout: 1000 });
    const sessionId = recorder.getSessionId();

    await vi.advanceTimersByTimeAsync(2000);
    expect(recorder.isPaused()).toBe(true);

    window.dispatchEvent(new Event("pointerdown"));
    await vi.waitFor(() => expect(recordCalls.length).toBe(2));

    expect(recorder.isPaused()).toBe(false);
    expect(recorder.getSessionId()).toBe(sessionId);
    // Resuming must not open a second session on the server.
    const inits = fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/session-replay/init"));
    expect(inits).toHaveLength(1);
  });

  it("pauses immediately when the tab is hidden", async () => {
    const { recorder } = await startedRecorder({ idleTimeout: 0 });

    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));

    expect(recorder.isPaused()).toBe(true);
  });

  it("keeps recording indefinitely when idleTimeout is 0 and the tab is visible", async () => {
    const { recorder } = await startedRecorder({ idleTimeout: 0 });

    await vi.advanceTimersByTimeAsync(120000);

    expect(recorder.isPaused()).toBe(false);
  });
});

describe("stop during session init", () => {
  it("does not leave a session behind when stop() lands mid-init", async () => {
    // Hold /init open so stop() lands while the request is in flight.
    let releaseInit: (value: { status: number; ok: boolean }) => void = () => {};
    fetchMock.mockImplementationOnce(
      () => new Promise((resolve) => { releaseInit = resolve; }),
    );

    const recorder = newRecorder({ enabled: true });
    await vi.waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/session-replay/init")),
      ).toHaveLength(1),
    );

    // Stop before init resolves — the shape a StrictMode double-mount or an
    // effect cleanup produces through the React binding.
    recorder.stop();
    releaseInit({ status: 201, ok: true });
    await vi.advanceTimersByTimeAsync(50);

    // Previously the stop early-returned, then init populated sessionId and
    // localStorage, stranding a server session that never receives an event.
    expect(recorder.getSessionId()).toBeNull();
    expect(localStorage.getItem("currentRecordingSessionId")).toBeNull();
    expect(recordCalls).toHaveLength(0);
  });

  it("does not attach a recorder when destroy() lands mid-init", async () => {
    let releaseInit: (value: { status: number; ok: boolean }) => void = () => {};
    fetchMock.mockImplementationOnce(
      () => new Promise((resolve) => { releaseInit = resolve; }),
    );

    const recorder = newRecorder({ enabled: true });
    await vi.waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/session-replay/init")),
      ).toHaveLength(1),
    );

    recorder.destroy();
    releaseInit({ status: 201, ok: true });
    await vi.advanceTimersByTimeAsync(50);

    expect(recordCalls).toHaveLength(0);
    expect(recorder.getSessionId()).toBeNull();
  });
});

describe("buffer bound", () => {
  it("drops the oldest events instead of growing without limit", async () => {
    // Never resolve, so nothing drains and the buffer has to defend itself.
    const { recorder, emit } = await startedRecorder({ batchSize: 100000, maxBufferedEvents: 10 });
    fetchMock.mockImplementation(() => new Promise(() => {}));

    for (let i = 0; i < 25; i++) emit(mkEvent(i));

    const internal = recorder as unknown as { pending: unknown[] };
    expect(internal.pending).toHaveLength(10);
    expect(recorder.getDroppedEventCount()).toBe(15);
  });
});
