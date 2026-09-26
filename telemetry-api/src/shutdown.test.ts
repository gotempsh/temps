// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, spyOn, beforeEach, afterEach, type Mock } from "bun:test";
import { gracefulShutdown } from "./shutdown.js";

// Silence shutdown logs for each test, and restore the real console afterwards
// so other test files (which share this process) keep their output.
let quiet: Mock<(...args: unknown[]) => void>[] = [];
beforeEach(() => {
  quiet = [spyOn(console, "log").mockImplementation(() => {}), spyOn(console, "error").mockImplementation(() => {})];
});
afterEach(() => quiet.forEach((s) => s.mockRestore()));

describe("gracefulShutdown", () => {
  it("waits for in-flight requests before flushing the backfill, then exits 0", async () => {
    const steps: string[] = [];
    let finishRequests!: () => void;
    const requestsDone = new Promise<void>((r) => (finishRequests = r));

    const done = gracefulShutdown(
      {
        stopServer: async () => {
          steps.push("server.stop called");
          await requestsDone;
          steps.push("requests drained");
        },
        stopBackfill: async () => void steps.push("backfill flushed"),
        pendingBackfill: () => 1,
        exit: (code) => void steps.push(`exit ${code}`),
      },
      "SIGTERM"
    );

    await Bun.sleep(20);
    // Nothing flushed or exited while a request is still in flight.
    expect(steps).toEqual(["server.stop called"]);

    finishRequests();
    await done;
    expect(steps).toEqual(["server.stop called", "requests drained", "backfill flushed", "exit 0"]);
  });

  it("exits 1 once the budget is exceeded, and never exits twice", async () => {
    const exits: number[] = [];
    await gracefulShutdown(
      {
        stopServer: () => Bun.sleep(80),
        stopBackfill: async () => {},
        pendingBackfill: () => 3,
        exit: (code) => void exits.push(code),
        budgetMs: 20,
      },
      "SIGTERM"
    );
    expect(exits).toEqual([1]);
  });

  it("still exits (1) when a shutdown step fails", async () => {
    const exits: number[] = [];
    await gracefulShutdown(
      {
        stopServer: async () => {
          throw new Error("stop failed");
        },
        stopBackfill: async () => {},
        pendingBackfill: () => 0,
        exit: (code) => void exits.push(code),
      },
      "SIGINT"
    );
    expect(exits).toEqual([1]);
  });
});
