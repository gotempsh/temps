// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

export interface AnalyticsEventBase {
  event_name: string;
  request_query?: string;
  request_path?: string;
  event_data?: Record<string, JsonValue>;
}

export interface SessionRecordingConfig {
  /**
   * Paths to exclude from recording. Supports `*` wildcards. Merged with the
   * built-in `DEFAULT_EXCLUDED_PATHS` (login, checkout, payment, etc.) unless
   * `useDefaultExcludedPaths` is set to `false`.
   */
  excludedPaths?: string[];
  /**
   * Whether to merge the built-in default excluded paths (see
   * `DEFAULT_EXCLUDED_PATHS`) with `excludedPaths`. Defaults to `true`. Set to
   * `false` to record every path except the ones you explicitly list in
   * `excludedPaths`.
   */
  useDefaultExcludedPaths?: boolean;
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
  /**
   * Milliseconds of no user interaction after which recording pauses. A paused
   * recorder detaches rrweb entirely, so a passive tab stops producing events
   * instead of recording page-driven DOM churn forever. Interaction resumes it
   * with a fresh full snapshot, on the same session. Set to `0` to never pause.
   * Defaults to 60000 (1 minute).
   */
  idleTimeout?: number;
  /**
   * Pause recording while the document is hidden (background tab). Defaults to
   * true. Independent of `idleTimeout` — a hidden tab pauses immediately.
   */
  pauseOnHidden?: boolean;
  /**
   * Force a full DOM snapshot every N milliseconds. Snapshots make replay
   * seeking cheap but are by far the largest events, so raising this trades
   * seek granularity for ingest volume. Defaults to 30000.
   */
  checkoutEveryNms?: number;
  /** Force a full DOM snapshot every N events. Defaults to 200. */
  checkoutEveryNth?: number;
  /**
   * Upper bound on events buffered while the server is unreachable. Past this
   * the oldest events are dropped, so a persistent ingest outage costs bounded
   * memory rather than growing until the tab dies. Defaults to 5000.
   */
  maxBufferedEvents?: number;
}

export interface AnalyticsClientOptions {
  /** Base endpoint path. Defaults to `/api/_temps`. */
  basePath?: string;
  /** Set to true to disable analytics (e.g., for tests). */
  disabled?: boolean;
  /** Ignore localhost/test env automatically. Defaults to true. */
  ignoreLocalhost?: boolean;
  /** Custom domain to use for analytics. Defaults to window.location.hostname. */
  domain?: string;
  /**
   * Analytics ingest key (`pa_…`), minted per project in the Temps Console
   * (Project → Analytics → Setup) or with
   * `bunx @temps-sdk/cli analytics keys create`.
   *
   * Only needed when Temps neither serves nor proxies the app. Without a
   * Temps-managed deployment there is no `Host` entry in the proxy route
   * table, so the server cannot tell which project an event belongs to and
   * rejects ingest outright. Presenting the key resolves project and
   * environment scope directly and the `Host` header stops being consulted
   * for attribution. Leave it unset for apps deployed by Temps — that path is
   * unchanged and remains the default.
   *
   * The key is **public by design**: it ships in your browser bundle and
   * appears in request URLs on the `sendBeacon` path. It grants analytics
   * ingest and nothing else. Never put a `tk_` API key, a `dt_` deployment
   * token or an `si_` service ingest token here — those are secrets and do
   * not work on this endpoint.
   */
  ingestKey?: string;
}

export interface AnalyticsOptions extends AnalyticsClientOptions {
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
}

export interface AnalyticsApi {
  /** Whether analytics are currently enabled. */
  readonly enabled: boolean;
  /**
   * The ingest key this instance was configured with, if any. Framework
   * hooks that send their own beacons outside the plugin instance (e.g.
   * `usePageLeave`'s unload handler) read this as their default so a
   * cross-origin setup only has to configure the key once, on the plugin —
   * not again on every hook that bypasses it.
   */
  readonly ingestKey?: string;
  /** Send a custom event. */
  trackEvent(eventName: string, data?: Record<string, JsonValue>): Promise<void>;
  /** Manually trigger a pageview. */
  trackPageview(): void;
  /** Identify a user. No-op placeholder for now. */
  identify(userId: string, traits?: Record<string, JsonValue>): Promise<void> | void;
  /** Enable session recording at runtime. */
  enableSessionRecording(): void;
  /** Disable session recording at runtime. */
  disableSessionRecording(): void;
  /** Tear down all listeners, timers, and recorders. */
  destroy(): void;
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
