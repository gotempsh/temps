// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useEffect } from "react";
import { SpeedTracker } from "@temps-sdk/analytics-core";

export interface UseSpeedAnalyticsOptions {
  /** Base endpoint path. Defaults to `/_temps`. */
  basePath?: string;
  /**
   * Analytics ingest key (`pa_…`). Required only when Temps does not serve or
   * proxy the app. See `AnalyticsClientOptions.ingestKey`.
   */
  ingestKey?: string;
  /** Set to true to disable speed analytics. Defaults to false. */
  disabled?: boolean;
}

/**
 * Reports Web Vitals for the page that mounted the hook. See `SpeedTracker`
 * for what is sent and when: load metrics never wait for an interaction, and
 * every beacon is attributed to the path captured at mount, not the path
 * current when a late CLS/INP value is reported.
 */
export function useSpeedAnalytics(options: UseSpeedAnalyticsOptions = {}): void {
  const { basePath = "/_temps", ingestKey, disabled = false } = options;

  useEffect(() => {
    if (disabled || typeof window === "undefined") return;
    const tracker = new SpeedTracker({ basePath, ingestKey });
    return () => tracker.destroy();
  }, [basePath, ingestKey, disabled]);
}
