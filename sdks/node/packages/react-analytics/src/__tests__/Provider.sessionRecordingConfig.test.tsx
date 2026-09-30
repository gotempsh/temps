// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import React from "react";

/** Options every core recorder was constructed with. */
const constructed: Array<Record<string, unknown>> = [];

vi.mock("@temps-sdk/analytics-core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@temps-sdk/analytics-core")>();
  class RecordingSpy {
    constructor(options: Record<string, unknown>) {
      constructed.push(options);
    }
    destroy(): void {}
  }
  return { ...actual, SessionRecorder: RecordingSpy };
});

import { TempsAnalyticsProvider } from "../Provider";

describe("TempsAnalyticsProvider sessionRecordingConfig", () => {
  beforeEach(() => {
    constructed.length = 0;
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, status: 200 }));
  });

  /**
   * Regression: the provider forwarded a hand-picked list of eleven fields and
   * silently dropped the rest, so e.g. `useDefaultExcludedPaths: false` could
   * not turn off the built-in exclusions through the provider.
   */
  it("forwards every recorder option, not a hand-picked subset", async () => {
    const config = {
      useDefaultExcludedPaths: false,
      idleTimeout: 0,
      pauseOnHidden: false,
      checkoutEveryNms: 5000,
      checkoutEveryNth: 50,
      maxBufferedEvents: 123,
      debug: true,
      blockSelector: "[data-secret]",
      ignoreSelector: "[data-noise]",
      maskInputOptions: { password: true, email: false },
      slimDOMOptions: { script: true },
      sampling: { scroll: 250 },
      excludedPaths: ["/admin/*"],
    };

    render(
      <TempsAnalyticsProvider
        basePath="/api/_temps"
        ignoreLocalhost={false}
        enableSessionRecording
        sessionRecordingConfig={config}
      >
        <div />
      </TempsAnalyticsProvider>,
    );

    await waitFor(() => expect(constructed.length).toBeGreaterThan(0));
    expect(constructed.at(-1)).toMatchObject({ ...config, basePath: "/api/_temps", enabled: true });
  });

  it("keeps the provider's own basePath and ingest key over the config", async () => {
    render(
      <TempsAnalyticsProvider
        basePath="/api/_temps"
        ingestKey="pa_provider"
        ignoreLocalhost={false}
        enableSessionRecording
        sessionRecordingConfig={
          { basePath: "/elsewhere", ingestKey: "pa_config" } as unknown as Record<string, never>
        }
      >
        <div />
      </TempsAnalyticsProvider>,
    );

    await waitFor(() => expect(constructed.length).toBeGreaterThan(0));
    expect(constructed.at(-1)).toMatchObject({ basePath: "/api/_temps", ingestKey: "pa_provider" });
  });
});
