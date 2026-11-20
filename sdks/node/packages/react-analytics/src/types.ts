// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type React from "react";
import type {
  JsonValue,
  AnalyticsClientOptions,
  AutoTrackOptions,
  FeatureFlags,
  FeatureFlagsOptions,
} from "@temps-sdk/analytics-core";
import type { SessionRecorderConfig } from "./SessionRecorder";

export type {
  JsonPrimitive,
  JsonValue,
  AnalyticsEventBase,
  AnalyticsClientOptions,
  WebVitalMetric,
  SpeedMetric,
} from "@temps-sdk/analytics-core";

export interface AnalyticsContextValue {
  /** Send a custom event. */
  trackEvent: (eventName: string, data?: Record<string, JsonValue>) => Promise<void>;
  /** Identify an user if needed. No-op by default. */
  identify: (userId: string, traits?: Record<string, JsonValue>) => Promise<void> | void;
  /** Manually trigger a pageview. */
  trackPageview: () => void;
  /**
   * Report the current page as missing: sends a `not_found` event whose
   * `path` property is the current path. Call it from your "not found" view,
   * or mark that view with `data-temps-404` and let the provider do it.
   */
  trackNotFound: (data?: Record<string, JsonValue>) => Promise<void>;
  /** Whether analytics are currently enabled. */
  enabled: boolean;
  /** Client-visible feature flags for this visitor. Prefer `useFlag`. */
  flags: FeatureFlags;
  /** Where the Temps endpoints live, as configured on the provider. */
  basePath: string;
}

export interface TempsAnalyticsProviderProps extends AnalyticsClientOptions, AutoTrackOptions {
  /**
   * Feature-flag context (`key`, `attributes`) and server-rendered
   * `bootstrap` values. Read flags with `useFlag`.
   */
  flags?: Pick<FeatureFlagsOptions, "key" | "attributes" | "bootstrap" | "onError">;
  /** Auto track pageviews on route changes. Defaults to true. */
  autoTrackPageviews?: boolean;
  /** Auto track page leave events. Defaults to true. */
  autoTrackPageLeave?: boolean;
  /** Custom event name for page leave events. Defaults to "page_leave". */
  pageLeaveEventName?: string;
  /** Auto track speed analytics (Web Vitals). Defaults to true. */
  autoTrackSpeedAnalytics?: boolean;
  /** Auto track engagement with heartbeats. Defaults to true. */
  autoTrackEngagement?: boolean;
  /** Heartbeat interval in milliseconds. Defaults to 30000 (30 seconds). */
  heartbeatInterval?: number;
  /** Inactivity timeout in milliseconds. Defaults to 30000 (30 seconds). */
  inactivityTimeout?: number;
  /** Engagement threshold in milliseconds to consider session engaged. Defaults to 10000 (10 seconds). */
  engagementThreshold?: number;
  /** Enable session recording. Defaults to false. */
  enableSessionRecording?: boolean;
  /**
   * Session recording configuration, forwarded to the recorder in full
   * (including `idleTimeout`, `useDefaultExcludedPaths`, `debug` and the
   * rrweb-level options).
   */
  sessionRecordingConfig?: SessionRecorderConfig;
  /** Children to render inside the provider. */
  children: React.ReactNode;
}
