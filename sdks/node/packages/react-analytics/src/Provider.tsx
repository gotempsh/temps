"use client";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef } from "react";
import type { AnalyticsContextValue, TempsAnalyticsProviderProps } from "./types";
import { isLocalhostLike, isTestEnvironment, sendAnalytics, sendAnalyticsReliable } from "./utils";
import { useSpeedAnalytics } from "./useSpeedAnalytics";
import { EngagementTracker } from "./EngagementTracker";
import { SessionRecorder } from "./SessionRecorder";
import { SessionRecordingProvider, useSessionRecordingControl } from "./useSessionRecording";
import { DEFAULT_BASE_PATH } from "./constants";

const AnalyticsContext = createContext<AnalyticsContextValue | undefined>(undefined);

export function TempsAnalyticsProvider({
  basePath = DEFAULT_BASE_PATH,
  disabled = false,
  ignoreLocalhost = true,
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
  domain,
  children,
}: TempsAnalyticsProviderProps) {
  const enabled = useMemo(() => {
    if (disabled) return false;
    if (typeof window === "undefined") return false;
    if (ignoreLocalhost && (isLocalhostLike() || isTestEnvironment())) return false;
    try {
      if (window.localStorage?.temps_ignore === "true") return false;
    } catch {}
    return true;
  }, [disabled, ignoreLocalhost]);

  const trackEvent = useCallback<AnalyticsContextValue["trackEvent"]>(
    async (eventName, data = {}) => {
      if (!enabled) return;
      await sendAnalytics("event", {
        event_name: eventName,
        request_query: window.location.search,
        request_path: window.location.pathname,
        domain: domain || window.location.hostname,
        event_data: data,
      }, "POST", basePath);
    },
    [enabled, basePath, domain]
  );

  const trackPageview = useCallback<AnalyticsContextValue["trackPageview"]>(
    () => {
      if (!enabled) return;
      void sendAnalytics("event", {
        event_name: "page_view",
        request_query: window.location.search,
        request_path: window.location.pathname,
        domain: domain || window.location.hostname,
        event_data: {
          referrer: document.referrer,
          userAgent: navigator.userAgent,
          timestamp: new Date().toISOString(),
        },
      }, "POST", basePath);
    },
    [enabled, basePath, domain]
  );

  const identify = useCallback<AnalyticsContextValue["identify"]>(async () => {
    // no-op placeholder for SDK parity; implement when identity endpoint is available
  }, []);

  // Route change monitoring (pushState/popstate)
  const currentPathRef = useRef<string>(typeof window !== "undefined" ? window.location.pathname : "");

  useEffect(() => {
    if (!autoTrackPageviews || !enabled) return;

    let initialLoad = true;
    const originalPushState = window.history?.pushState?.bind(window.history) as History["pushState"] | undefined;

    function maybeTrack() {
      const nextPath = window.location.pathname;
      if (currentPathRef.current !== nextPath) {
        currentPathRef.current = nextPath;
        trackPageview();
      }
    }

    if (originalPushState) {
      window.history.pushState = ((data: any, unused: string, url?: string | URL | null) => {
        originalPushState(data, unused, url as any);
        maybeTrack();
      }) as History["pushState"];

      const onPop = () => maybeTrack();
      window.addEventListener("popstate", onPop);

      const cleanup = () => {
        window.removeEventListener("popstate", onPop);
        if (originalPushState) {
          window.history.pushState = originalPushState;
        }
      };

      // Initial load or prerender visibility handling
      if ((document.visibilityState as unknown as string) === "prerender") {
        const onVisibility = () => {
          if (document.visibilityState === "visible") {
            if (initialLoad) {
              initialLoad = false;
              trackPageview();
            }
            document.removeEventListener("visibilitychange", onVisibility);
          }
        };
        document.addEventListener("visibilitychange", onVisibility);
        return () => {
          document.removeEventListener("visibilitychange", onVisibility);
          cleanup();
        };
      } else if (initialLoad) {
        initialLoad = false;
        trackPageview();
      }

      return cleanup;
    }
  }, [autoTrackPageviews, enabled, trackPageview]);

  // Click delegation for [temps-event-name] with temps-data-* attributes
  useEffect(() => {
    if (!enabled) return;
    const onClick = (event: MouseEvent) => {
      const target = event.target as Element | null;
      const eventElement = target?.closest?.("[temps-event-name]");
      if (!(eventElement instanceof HTMLElement)) return;

      const eventName = eventElement.getAttribute("temps-event-name");
      if (!eventName) return;

      const eventData: Record<string, import("./types").JsonValue> = {};
      for (const attr of eventElement.getAttributeNames()) {
        if (attr.startsWith("temps-data-")) {
          const dataKey = attr.replace("temps-data-", "");
          eventData[dataKey] = eventElement.getAttribute(attr);
        }
      }

      void trackEvent(eventName, eventData);
    };
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, [enabled, trackEvent]);

  // Engagement tracking with heartbeats and page leave
  useEffect(() => {
    if (!enabled) return;

    // If engagement tracking is enabled, it handles both heartbeats and page leave
    if (autoTrackEngagement) {
      const tracker = new EngagementTracker({
        basePath,
        domain,
        heartbeatInterval,
        inactivityTimeout,
        engagementThreshold,
      });

      return () => {
        tracker.destroy();
      };
    }
    // Legacy page leave tracking without engagement metrics
    else if (autoTrackPageLeave) {
      let hasTracked = false;
      let startTime = Date.now();

      const trackPageLeave = () => {
        if (hasTracked) return;
        hasTracked = true;

        const timeOnPage = Date.now() - startTime;

        sendAnalyticsReliable("event", {
          event_name: pageLeaveEventName,
          request_query: window.location.search,
          request_path: window.location.pathname,
          domain: domain || window.location.hostname,
          event_data: {
            time_on_page_ms: timeOnPage,
            timestamp: new Date().toISOString(),
            url: window.location.href,
            referrer: document.referrer,
          },
        }, basePath);
      };

      // Use pagehide as primary (most reliable), with beforeunload as fallback
      const handlePageLeave = () => trackPageLeave();

      // pagehide is the most reliable for modern browsers
      window.addEventListener("pagehide", handlePageLeave);
      // beforeunload as fallback for older browsers
      window.addEventListener("beforeunload", handlePageLeave);

      return () => {
        window.removeEventListener("pagehide", handlePageLeave);
        window.removeEventListener("beforeunload", handlePageLeave);
      };
    }
  }, [autoTrackPageLeave, autoTrackEngagement, enabled, pageLeaveEventName, domain, basePath, heartbeatInterval, inactivityTimeout, engagementThreshold]);

  // Speed analytics tracking
  useSpeedAnalytics({
    basePath,
    disabled: !enabled || !autoTrackSpeedAnalytics,
  });

  // Session recording control
  const { isEnabled: isRecordingEnabled } = useSessionRecordingControl(enableSessionRecording);

  const value = useMemo<AnalyticsContextValue>(
    () => ({ trackEvent, identify, trackPageview, enabled }),
    [trackEvent, identify, trackPageview, enabled]
  );

  return (
    <AnalyticsContext.Provider value={value}>
      <SessionRecordingProvider defaultEnabled={enableSessionRecording}>
        {children}
        {enabled && (
          <SessionRecorder
            basePath={basePath}
            domain={domain}
            enabled={isRecordingEnabled}
            excludedPaths={sessionRecordingConfig.excludedPaths}
            sessionSampleRate={sessionRecordingConfig.sessionSampleRate}
            maskAllInputs={sessionRecordingConfig.maskAllInputs}
            maskTextSelector={sessionRecordingConfig.maskTextSelector}
            blockClass={sessionRecordingConfig.blockClass}
            ignoreClass={sessionRecordingConfig.ignoreClass}
            maskTextClass={sessionRecordingConfig.maskTextClass}
            recordCanvas={sessionRecordingConfig.recordCanvas}
            collectFonts={sessionRecordingConfig.collectFonts}
            batchSize={sessionRecordingConfig.batchSize}
            flushInterval={sessionRecordingConfig.flushInterval}
          />
        )}
      </SessionRecordingProvider>
    </AnalyticsContext.Provider>
  );
}

export function useTempsAnalytics(): AnalyticsContextValue {
  const ctx = useContext(AnalyticsContext);
  if (!ctx) {
    throw new Error("useTempsAnalytics must be used within a TempsAnalyticsProvider");
  }
  return ctx;
}
