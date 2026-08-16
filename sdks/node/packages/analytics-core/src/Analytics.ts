import { DEFAULT_BASE_PATH } from "./constants";
import type { AnalyticsApi, AnalyticsOptions, JsonValue } from "./types";
import {
  isLocalhostLike,
  isTestEnvironment,
  sendAnalytics,
  sendAnalyticsReliable,
} from "./utils";
import { EngagementTracker } from "./EngagementTracker";
import { SpeedTracker } from "./SpeedTracker";
import { SessionRecorder } from "./SessionRecorder";

interface InternalCleanup {
  (): void;
}

/**
 * Framework-agnostic factory. Call once at app startup in the browser to get an
 * analytics instance. All framework adapters wrap this factory.
 */
export function createAnalytics(options: AnalyticsOptions = {}): AnalyticsApi {
  const {
    basePath = DEFAULT_BASE_PATH,
    disabled = false,
    ignoreLocalhost = true,
    domain,
    autoTrackPageviews = true,
    autoTrackPageLeave = true,
    pageLeaveEventName = "page_leave",
    autoTrackSpeedAnalytics = true,
    autoTrackEngagement = true,
    heartbeatInterval = 30000,
    inactivityTimeout = 30000,
    engagementThreshold = 10000,
    enableSessionRecording = false,
    sessionRecordingConfig = {},
  } = options;

  const cleanups: InternalCleanup[] = [];

  const enabled = computeEnabled(disabled, ignoreLocalhost);

  const resolveDomain = (): string =>
    domain || (typeof window !== "undefined" ? window.location.hostname : "");

  const trackEvent: AnalyticsApi["trackEvent"] = async (eventName, data = {}) => {
    if (!enabled) return;
    await sendAnalytics(
      "event",
      {
        event_name: eventName,
        request_query: window.location.search,
        request_path: window.location.pathname,
        domain: resolveDomain(),
        language: navigator.language,
        event_data: data as Record<string, JsonValue>,
      },
      "POST",
      basePath
    );
  };

  const trackPageview: AnalyticsApi["trackPageview"] = () => {
    if (!enabled) return;
    void sendAnalytics(
      "event",
      {
        event_name: "page_view",
        request_query: window.location.search,
        request_path: window.location.pathname,
        domain: resolveDomain(),
        language: navigator.language,
        event_data: {
          referrer: document.referrer,
          userAgent: navigator.userAgent,
          timestamp: new Date().toISOString(),
        },
      },
      "POST",
      basePath
    );
  };

  const identify: AnalyticsApi["identify"] = async () => {
    // Placeholder for SDK parity; implement when identity endpoint is available.
  };

  if (enabled) {
    if (autoTrackPageviews) {
      cleanups.push(setupPageviewTracking(trackPageview));
    }

    if (autoTrackEngagement) {
      const tracker = new EngagementTracker({
        basePath,
        domain: resolveDomain(),
        heartbeatInterval,
        inactivityTimeout,
        engagementThreshold,
      });
      cleanups.push(() => tracker.destroy());
    } else if (autoTrackPageLeave) {
      cleanups.push(setupLegacyPageLeave(pageLeaveEventName, resolveDomain, basePath));
    }

    if (autoTrackSpeedAnalytics) {
      const speed = new SpeedTracker({ basePath });
      cleanups.push(() => speed.destroy());
    }

    cleanups.push(setupClickDelegation(trackEvent));
  }

  const recorder =
    enabled && enableSessionRecording
      ? new SessionRecorder({ basePath, domain: resolveDomain(), enabled: true, ...sessionRecordingConfig })
      : null;
  if (recorder) cleanups.push(() => recorder.destroy());

  // Runtime-controlled recorder handle for enable/disable after init
  let runtimeRecorder: SessionRecorder | null = recorder;

  return {
    get enabled(): boolean {
      return enabled;
    },
    trackEvent,
    trackPageview,
    identify,
    enableSessionRecording(): void {
      if (!enabled) return;
      if (runtimeRecorder) {
        runtimeRecorder.start();
        return;
      }
      runtimeRecorder = new SessionRecorder({
        basePath,
        domain: resolveDomain(),
        enabled: true,
        ...sessionRecordingConfig,
      });
      cleanups.push(() => runtimeRecorder?.destroy());
    },
    disableSessionRecording(): void {
      runtimeRecorder?.stop();
    },
    destroy(): void {
      while (cleanups.length > 0) {
        const fn = cleanups.pop();
        try {
          fn?.();
        } catch (error) {
          // eslint-disable-next-line no-console
          console.error("Analytics cleanup error:", error);
        }
      }
    },
  };
}

function computeEnabled(disabled: boolean, ignoreLocalhost: boolean): boolean {
  if (disabled) return false;
  if (typeof window === "undefined") return false;
  if (ignoreLocalhost && (isLocalhostLike() || isTestEnvironment())) return false;
  try {
    if ((window.localStorage as Storage | undefined)?.getItem("temps_ignore") === "true") return false;
    // Legacy lookup via property (matches old React implementation)
    const legacy = (window.localStorage as unknown as Record<string, unknown>)?.temps_ignore;
    if (legacy === "true") return false;
  } catch {
    // ignore
  }
  return true;
}

function setupPageviewTracking(trackPageview: () => void): InternalCleanup {
  let currentPath = window.location.pathname;
  let initialLoad = true;

  const originalPushState = window.history.pushState.bind(window.history);

  const maybeTrack = (): void => {
    const nextPath = window.location.pathname;
    if (currentPath !== nextPath) {
      currentPath = nextPath;
      trackPageview();
    }
  };

  window.history.pushState = ((
    data: unknown,
    unused: string,
    url?: string | URL | null
  ) => {
    originalPushState(data as never, unused, url as never);
    maybeTrack();
  }) as History["pushState"];

  const onPop = (): void => maybeTrack();
  window.addEventListener("popstate", onPop);

  const handleVisibility = (): void => {
    if (document.visibilityState === "visible" && initialLoad) {
      initialLoad = false;
      trackPageview();
      document.removeEventListener("visibilitychange", handleVisibility);
    }
  };

  if ((document.visibilityState as unknown as string) === "prerender") {
    document.addEventListener("visibilitychange", handleVisibility);
  } else if (initialLoad) {
    initialLoad = false;
    trackPageview();
  }

  return (): void => {
    window.removeEventListener("popstate", onPop);
    document.removeEventListener("visibilitychange", handleVisibility);
    window.history.pushState = originalPushState;
  };
}

function setupClickDelegation(
  trackEvent: (name: string, data?: Record<string, JsonValue>) => Promise<void>
): InternalCleanup {
  const onClick = (event: MouseEvent): void => {
    const target = event.target as Element | null;
    const eventElement = target?.closest?.("[temps-event-name]");
    if (!(eventElement instanceof HTMLElement)) return;

    const eventName = eventElement.getAttribute("temps-event-name");
    if (!eventName) return;

    const eventData: Record<string, JsonValue> = {};
    for (const attr of eventElement.getAttributeNames()) {
      if (attr.startsWith("temps-data-")) {
        const dataKey = attr.replace("temps-data-", "");
        eventData[dataKey] = eventElement.getAttribute(attr);
      }
    }

    void trackEvent(eventName, eventData);
  };

  document.addEventListener("click", onClick);
  return (): void => document.removeEventListener("click", onClick);
}

function setupLegacyPageLeave(
  pageLeaveEventName: string,
  resolveDomain: () => string,
  basePath: string
): InternalCleanup {
  let hasTracked = false;
  const startTime = Date.now();

  const trackPageLeave = (): void => {
    if (hasTracked) return;
    hasTracked = true;
    const timeOnPage = Date.now() - startTime;
    sendAnalyticsReliable(
      "event",
      {
        event_name: pageLeaveEventName,
        request_query: window.location.search,
        request_path: window.location.pathname,
        domain: resolveDomain(),
        language: navigator.language,
        event_data: {
          time_on_page_ms: timeOnPage,
          timestamp: new Date().toISOString(),
          url: window.location.href,
          referrer: document.referrer,
        },
      },
      basePath
    );
  };

  window.addEventListener("pagehide", trackPageLeave);
  window.addEventListener("beforeunload", trackPageLeave);

  return (): void => {
    window.removeEventListener("pagehide", trackPageLeave);
    window.removeEventListener("beforeunload", trackPageLeave);
  };
}
