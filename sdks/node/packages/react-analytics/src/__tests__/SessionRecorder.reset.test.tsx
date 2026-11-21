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

  it("ends a mounted recording, resumes it after the reset, and stops listening once unmounted", async () => {
    vi.spyOn(global, "fetch").mockImplementation(async () => new Response(null, { status: 204 }));
    const end = vi.spyOn(CoreSessionRecorder.prototype, "endForReset");
    const resume = vi.spyOn(CoreSessionRecorder.prototype, "resumeAfterReset");

    const mounted = render(
      <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
    );
    expect(end).not.toHaveBeenCalled();

    await resetVisitor({ basePath: DEFAULT_BASE_PATH });
    expect(end).toHaveBeenCalledTimes(1);
    expect(resume).toHaveBeenCalledTimes(1);

    mounted.unmount();
    await resetVisitor({ basePath: DEFAULT_BASE_PATH });
    expect(end).toHaveBeenCalledTimes(1);
  });
});
