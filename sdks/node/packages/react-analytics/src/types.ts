import type React from "react";
export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

export interface AnalyticsEventBase {
  event_name: string;
  request_query?: string;
  request_path?: string;
  event_data?: Record<string, JsonValue>;
}

export interface AnalyticsClientOptions {
  /** Base endpoint path. Defaults to `/_temps`. */
  basePath?: string;
  /** Set to true to disable analytics (e.g., for tests). */
  disabled?: boolean;
  /** Ignore localhost/test env automatically. Defaults to true. */
  ignoreLocalhost?: boolean;
  /** Custom domain to use for analytics. Defaults to window.location.hostname. */
  domain?: string;
}

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
  sessionRecordingConfig?: {
    /** Paths to exclude from recording. */
    excludedPaths?: string[];
    /** Sample rate for recording sessions (0.0 to 1.0). Defaults to 1.0. */
    sessionSampleRate?: number;
    /** Mask all inputs. Defaults to true. */
    maskAllInputs?: boolean;
    /** CSS selector for masking text. Defaults to "[data-mask]". */
    maskTextSelector?: string;
    /** CSS class to block from recording. Defaults to "rr-block". */
    blockClass?: string;
    /** CSS class to ignore from recording. Defaults to "rr-ignore". */
    ignoreClass?: string;
    /** CSS class to mask text. Defaults to "rr-mask". */
    maskTextClass?: string;
    /** Record canvas elements. Defaults to false. */
    recordCanvas?: boolean;
    /** Collect fonts. Defaults to false. */
    collectFonts?: boolean;
    /** Number of events to batch before sending. Defaults to 100. */
    batchSize?: number;
    /** Interval in ms to flush events. Defaults to 5000. */
    flushInterval?: number;
  };
  /** Children to render inside the provider. */
  children: React.ReactNode;
}

export interface WebVitalMetric {
  value: number;
  rating: "good" | "needs-improvement" | "poor";
}

export interface SpeedMetric {
  ttfb?: number | null;
  lcp?: number | null;
  fid?: number | null;
  fcp?: number | null;
  cls?: number | null;
  inp?: number | null;
  path?: string | null;
  query?: string | null;
}
