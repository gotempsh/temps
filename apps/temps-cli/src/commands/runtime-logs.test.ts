// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from "bun:test";
import {
  parseDeploymentId,
  runtimeLogConnectionFailed,
  selectRuntimeLogContainer,
} from "./runtime-logs.js";

const containers = [
  { container_id: "abc123456789", container_name: "checkout-production" },
  { container_id: "def987654321", container_name: "worker-production" },
];

describe("selectRuntimeLogContainer", () => {
  test("defaults to the first live container", () => {
    expect(selectRuntimeLogContainer(containers)).toEqual(containers[0]);
  });

  test("accepts partial IDs and names", () => {
    expect(selectRuntimeLogContainer(containers, "def987")).toEqual(
      containers[1],
    );
    expect(selectRuntimeLogContainer(containers, "checkout")).toEqual(
      containers[0],
    );
  });

  test("does not silently select a different container", () => {
    expect(selectRuntimeLogContainer(containers, "missing")).toBeUndefined();
  });
});

describe("parseDeploymentId", () => {
  test("accepts a complete positive integer", () => {
    expect(parseDeploymentId("25")).toBe(25);
  });

  test.each(["", "0", "-1", "25garbage", "1.5", " 25"])(
    "rejects malformed deployment ID %p",
    (value) => {
      expect(parseDeploymentId(value)).toBeUndefined();
    },
  );

  test("rejects integers that are not safe in JavaScript", () => {
    expect(parseDeploymentId("9007199254740992")).toBeUndefined();
  });
});

describe("runtimeLogConnectionFailed", () => {
  test("treats a clean WebSocket close as success", () => {
    expect(runtimeLogConnectionFailed(1000, false)).toBeFalse();
  });

  test("treats transport errors and abnormal closes as failures", () => {
    expect(runtimeLogConnectionFailed(1000, true)).toBeTrue();
    expect(runtimeLogConnectionFailed(1006, false)).toBeTrue();
  });

  test("does not infer transport state from application log contents", () => {
    expect(runtimeLogConnectionFailed(1000, false)).toBeFalse();
    expect(runtimeLogConnectionFailed(1011, false)).toBeTrue();
  });
});
