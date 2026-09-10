// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useEffect } from "react";
import { onCLS, onFID, onLCP, onTTFB, onFCP, onINP, type Metric } from "web-vitals";
import { sendAnalytics } from "./utils";
import type { SpeedMetric, WebVitalMetric, JsonValue } from "./types";

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

export function useSpeedAnalytics(options: UseSpeedAnalyticsOptions = {}) {
  const { basePath = "/_temps", ingestKey, disabled = false } = options;

  useEffect(() => {
    if (disabled || typeof window === "undefined") return;

    const initialMetrics: Record<string, WebVitalMetric> = {};
    const lateMetrics: Record<string, WebVitalMetric> = {};

    const sendInitialMetrics = () => {
      if (Object.keys(initialMetrics).length === 4) {
        const metricsPayload = {
          ttfb: initialMetrics.TTFB?.value ?? null,
          lcp: initialMetrics.LCP?.value ?? null,
          fid: initialMetrics.FID?.value ?? null,
          fcp: initialMetrics.FCP?.value ?? null,
          // The ingest endpoint's field is `pathname`; a `path` key is not
          // recognized and the page ends up NULL in storage, breaking
          // per-page performance breakdowns.
          pathname: window.location.pathname,
          query: window.location.search,
        } as Record<string, JsonValue>;
        sendAnalytics("speed", metricsPayload, "POST", basePath, ingestKey);
      }
    };

    const sendLateMetric = (metricName: string, value: number) => {
      const payload = {
        [metricName.toLowerCase()]: value,
        pathname: window.location.pathname,
        query: window.location.search,
      } as Record<string, JsonValue>;
      sendAnalytics("speed", payload, "POST", basePath, ingestKey);
    };

    // Track metrics that can be gathered quickly
    onTTFB((metric: Metric) => {
      initialMetrics.TTFB = { value: metric.value, rating: metric.rating };
      sendInitialMetrics();
    });

    onLCP((metric: Metric) => {
      initialMetrics.LCP = { value: metric.value, rating: metric.rating };
      sendInitialMetrics();
    });

    onFID((metric: Metric) => {
      initialMetrics.FID = { value: metric.value, rating: metric.rating };
      sendInitialMetrics();
    });

    onFCP((metric: Metric) => {
      initialMetrics.FCP = { value: metric.value, rating: metric.rating };
      sendInitialMetrics();
    });

    // Track metrics that take longer to stabilize
    onCLS((metric: Metric) => {
      lateMetrics.CLS = { value: metric.value, rating: metric.rating };
      sendLateMetric("cls", metric.value);
    });

    onINP((metric: Metric) => {
      lateMetrics.INP = { value: metric.value, rating: metric.rating };
      sendLateMetric("inp", metric.value);
    });
  }, [basePath, ingestKey, disabled]);
}
