// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { onCLS, onFID, onLCP, onTTFB, onFCP, onINP, type Metric } from "web-vitals";
import { sendAnalytics } from "./utils";
import type { JsonValue, WebVitalMetric } from "./types";

export interface SpeedTrackerOptions {
  basePath: string;
  /** Analytics ingest key (`pa_…`). See `AnalyticsClientOptions.ingestKey`. */
  ingestKey?: string;
}

/**
 * Subscribes to Web Vitals and forwards metrics to the Temps analytics endpoint.
 * Initial metrics (TTFB, FCP, LCP, FID) are batched into a single "speed" request.
 * Late metrics (CLS, INP) are sent individually as they stabilize.
 */
export class SpeedTracker {
  private readonly basePath: string;
  private readonly ingestKey?: string;
  private initialMetrics: Record<string, WebVitalMetric> = {};

  constructor(options: SpeedTrackerOptions) {
    this.basePath = options.basePath;
    this.ingestKey = options.ingestKey;
    if (typeof window === "undefined") return;
    this.start();
  }

  private start(): void {
    onTTFB((m: Metric) => {
      this.initialMetrics.TTFB = { value: m.value, rating: m.rating };
      this.sendInitial();
    });
    onLCP((m: Metric) => {
      this.initialMetrics.LCP = { value: m.value, rating: m.rating };
      this.sendInitial();
    });
    onFID((m: Metric) => {
      this.initialMetrics.FID = { value: m.value, rating: m.rating };
      this.sendInitial();
    });
    onFCP((m: Metric) => {
      this.initialMetrics.FCP = { value: m.value, rating: m.rating };
      this.sendInitial();
    });

    onCLS((m: Metric) => this.sendLate("cls", m.value));
    onINP((m: Metric) => this.sendLate("inp", m.value));
  }

  private sendInitial(): void {
    if (Object.keys(this.initialMetrics).length !== 4) return;
    const payload = {
      ttfb: this.initialMetrics.TTFB?.value ?? null,
      lcp: this.initialMetrics.LCP?.value ?? null,
      fid: this.initialMetrics.FID?.value ?? null,
      fcp: this.initialMetrics.FCP?.value ?? null,
      path: window.location.pathname,
      query: window.location.search,
    } as Record<string, JsonValue>;
    void sendAnalytics("speed", payload, "POST", this.basePath, this.ingestKey);
  }

  private sendLate(name: string, value: number): void {
    const payload = {
      [name]: value,
      path: window.location.pathname,
      query: window.location.search,
    } as Record<string, JsonValue>;
    void sendAnalytics("speed", payload, "POST", this.basePath, this.ingestKey);
  }

  // Web-vitals subscriptions are fire-and-forget; there's nothing to tear down.
  public destroy(): void {
    this.initialMetrics = {};
  }
}
