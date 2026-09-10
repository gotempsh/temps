// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type React from "react";
import type {
  JsonValue,
  AnalyticsClientOptions,
  SessionRecordingConfig,
} from "@temps-sdk/analytics-core";

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
  /** Whether analytics are currently enabled. */
  enabled: boolean;
}

export interface TempsAnalyticsProviderProps extends AnalyticsClientOptions {
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
  /** Session recording configuration. */
  sessionRecordingConfig?: SessionRecordingConfig;
  /** Children to render inside the provider. */
  children: React.ReactNode;
}
