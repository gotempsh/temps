// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Its own file: the switch to cookieless is one-way for the module registry,
// and this test needs a recorder mounted before the switch.

import { describe, it, expect, vi, afterEach } from "vitest";
import { render } from "@testing-library/react";
import {
  SessionRecorder as CoreSessionRecorder,
  enableCookielessIdentity,
} from "@temps-sdk/analytics-core";
import { SessionRecorder } from "../SessionRecorder";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("SessionRecorder when the page switches to cookieless (ADR 109 D3)", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("discards a mounted recorder and starts none afterwards", () => {
    vi.spyOn(global, "fetch").mockResolvedValue(new Response(null, { status: 201 }));
    const discard = vi.spyOn(CoreSessionRecorder.prototype, "discard");
    const destroy = vi.spyOn(CoreSessionRecorder.prototype, "destroy");

    const mounted = render(
      <SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />,
    );
    expect(discard).not.toHaveBeenCalled();

    enableCookielessIdentity();
    expect(discard).toHaveBeenCalledTimes(1);

    // Unmounting does not touch the discarded recorder again.
    const destroysAfterDiscard = destroy.mock.calls.length;
    mounted.unmount();
    expect(destroy.mock.calls.length).toBe(destroysAfterDiscard);

    // A recorder mounted on the cookieless page never starts.
    const start = vi.spyOn(CoreSessionRecorder.prototype, "start");
    render(<SessionRecorder enabled={true} basePath={DEFAULT_BASE_PATH} domain="example.com" />);
    expect(start).not.toHaveBeenCalled();
    expect(discard).toHaveBeenCalledTimes(1);
  });
});
