// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi, afterEach } from "vitest";
import { render } from "@testing-library/react";
import { SessionRecorder as CoreSessionRecorder, resetVisitor } from "@temps-sdk/analytics-core";
import { SessionRecorder } from "../SessionRecorder";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("SessionRecorder when the visitor resets (ADR 109 D4)", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("restarts a mounted recorder, and stops listening once unmounted", async () => {
    vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 204 }));
    const restart = vi.spyOn(CoreSessionRecorder.prototype, "restart");

    const mounted = render(
      <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
    );
    expect(restart).not.toHaveBeenCalled();

    await resetVisitor({ basePath: DEFAULT_BASE_PATH });
    expect(restart).toHaveBeenCalledTimes(1);

    mounted.unmount();
    await resetVisitor({ basePath: DEFAULT_BASE_PATH });
    expect(restart).toHaveBeenCalledTimes(1);
  });
});
