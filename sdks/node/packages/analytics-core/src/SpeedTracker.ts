// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { onCLS, onFID, onLCP, onTTFB, onFCP, onINP, type Metric } from "web-vitals";
import { sendAnalytics, sendAnalyticsReliable } from "./utils";
import type { JsonValue } from "./types";

export interface SpeedTrackerOptions {
  basePath: string;
  /** Analytics ingest key (`pa_…`). See `AnalyticsClientOptions.ingestKey`. */
  ingestKey?: string;
}

type LoadMetricName = "ttfb" | "fcp" | "lcp" | "fid";

/**
 * Subscribes to Web Vitals and forwards metrics to the Temps analytics endpoint.
 *
 * Load metrics (TTFB, FCP, LCP) go out in one "speed" request as soon as all
 * three are known, or when the page is hidden, whichever comes first. They
 * never wait for an interaction: FID only exists for visitors who click or
 * type, so gating on it dropped every visitor who just read the page and
 * left. FID rides along when it already arrived, and is otherwise sent on its
 * own. Late metrics (CLS, INP) are sent individually as they stabilize.
 *
 * Every beacon carries the path and query captured when tracking started, not
 * the ones current at send time. Web Vitals here are page-lifetime metrics of
 * the hard navigation that loaded the page, so a CLS or INP value reported
 * after a client-side route change still belongs to the landing page — the
 * same attribution CrUX uses.
 */
export class SpeedTracker {
  private readonly basePath: string;
  private readonly ingestKey?: string;
  private readonly pathname: string = "";
  private readonly query: string = "";
  private loadMetrics: Partial<Record<LoadMetricName, number>> = {};
  private loadSent = false;
  private stopped = false;

  private readonly handleVisibilityChange = (): void => {
    if (document.visibilityState === "hidden") this.flushLoad();
  };

  private readonly handlePageHide = (): void => {
    this.flushLoad();
  };

  constructor(options: SpeedTrackerOptions) {
    this.basePath = options.basePath;
    this.ingestKey = options.ingestKey;
    if (typeof window === "undefined") return;
    this.pathname = window.location.pathname;
    this.query = window.location.search;
    this.start();
  }

  private start(): void {
    onTTFB((m: Metric) => this.recordLoad("ttfb", m.value));
    onFCP((m: Metric) => this.recordLoad("fcp", m.value));
    onLCP((m: Metric) => this.recordLoad("lcp", m.value));
    onFID((m: Metric) => this.recordFid(m.value));

    onCLS((m: Metric) => this.sendLate("cls", m.value));
    onINP((m: Metric) => this.sendLate("inp", m.value));

    // LCP is only final once the visitor interacts or leaves. A visitor who
    // leaves before it is reported still has a TTFB and FCP worth keeping.
    document.addEventListener("visibilitychange", this.handleVisibilityChange, true);
    window.addEventListener("pagehide", this.handlePageHide, true);
  }

  private recordLoad(name: Exclude<LoadMetricName, "fid">, value: number): void {
    if (this.stopped || this.loadSent) return;
    this.loadMetrics[name] = value;
    const { ttfb, fcp, lcp } = this.loadMetrics;
    if (ttfb !== undefined && fcp !== undefined && lcp !== undefined) {
      this.flushLoad();
    }
  }

  private recordFid(value: number): void {
    if (this.stopped) return;
    if (this.loadSent) {
      this.sendLate("fid", value);
      return;
    }
    this.loadMetrics.fid = value;
  }

  /** Send the load beacon once, with whatever load metrics are known. */
  private flushLoad(): void {
    if (this.stopped || this.loadSent) return;
    const { ttfb, fcp, lcp, fid } = this.loadMetrics;
    if (ttfb === undefined && fcp === undefined && lcp === undefined && fid === undefined) {
      return;
    }
    this.loadSent = true;
    this.send({
      ttfb: ttfb ?? null,
      lcp: lcp ?? null,
      fid: fid ?? null,
      fcp: fcp ?? null,
    });
  }

  private sendLate(name: string, value: number): void {
    if (this.stopped) return;
    this.send({ [name]: value });
  }

  private send(metrics: Record<string, JsonValue>): void {
    // The ingest field is `pathname`; a `path` key is ignored by the server
    // and leaves the page NULL in storage.
    const payload: Record<string, JsonValue> = {
      ...metrics,
      pathname: this.pathname,
      query: this.query,
    };
    // A page that is going away can cancel a plain fetch; a beacon survives.
    if (typeof document !== "undefined" && document.visibilityState === "hidden") {
      sendAnalyticsReliable("speed", payload, this.basePath, this.ingestKey);
      return;
    }
    void sendAnalytics("speed", payload, "POST", this.basePath, this.ingestKey);
  }

  /**
   * Stop sending. web-vitals has no unsubscribe, so its callbacks keep
   * firing; they are ignored from here on.
   */
  public destroy(): void {
    this.stopped = true;
    this.loadMetrics = {};
    if (typeof window === "undefined") return;
    document.removeEventListener("visibilitychange", this.handleVisibilityChange, true);
    window.removeEventListener("pagehide", this.handlePageHide, true);
  }
}
