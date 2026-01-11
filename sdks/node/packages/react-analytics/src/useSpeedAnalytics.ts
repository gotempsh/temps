"use client";
import { useEffect } from "react";
import { onCLS, onFID, onLCP, onTTFB, onFCP, onINP, type Metric } from "web-vitals";
import { sendAnalytics } from "./utils";
import type { SpeedMetric, WebVitalMetric, JsonValue } from "./types";

export interface UseSpeedAnalyticsOptions {
  /** Base endpoint path. Defaults to `/_temps`. */
  basePath?: string;
  /** Set to true to disable speed analytics. Defaults to false. */
  disabled?: boolean;
}

export function useSpeedAnalytics(options: UseSpeedAnalyticsOptions = {}) {
  const { basePath = "/_temps", disabled = false } = options;

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
          path: window.location.pathname,
          query: window.location.search,
        } as Record<string, JsonValue>;
        sendAnalytics("speed", metricsPayload, "POST", basePath);
      }
    };

    const sendLateMetric = (metricName: string, value: number) => {
      const payload = {
        [metricName.toLowerCase()]: value,
        path: window.location.pathname,
        query: window.location.search,
      } as Record<string, JsonValue>;
      sendAnalytics("speed", payload, "POST", basePath);
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
  }, [basePath, disabled]);
}
