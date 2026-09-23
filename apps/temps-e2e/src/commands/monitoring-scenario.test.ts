// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from "bun:test";
import { autoMonitorReady } from "./monitoring-scenario.ts";

describe("autoMonitorReady", () => {
  test("waits while the environment creation event is pending", () => {
    expect(autoMonitorReady([], 12)).toBe(false);
    expect(autoMonitorReady([{ environment_id: 12 }], 12)).toBe(true);
  });

  test("fails immediately when the monitor belongs to another environment", () => {
    expect(() => autoMonitorReady([{ environment_id: 9 }], 12)).toThrow(
      "auto-created monitor environment_id=9, expected 12",
    );
  });

  test("fails immediately when duplicate monitors were created", () => {
    expect(() =>
      autoMonitorReady([{ environment_id: 12 }, { environment_id: 12 }], 12),
    ).toThrow(
      "expected exactly 1 auto-created monitor for a fresh project, got 2",
    );
  });
});
