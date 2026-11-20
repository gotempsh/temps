// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import {
  NOT_FOUND_EVENT,
  NotFoundMarkerWatcher,
  notFoundEventData,
  setupLinkTracking,
} from "./AutoTracking";
import { enableCookielessIdentity, isCookielessIdentity, onCookielessIdentity } from "./identity";
import { exposureEventData, FeatureFlags } from "./FeatureFlags";
import { Surveys, type Survey } from "./Surveys";
import { renderSurveyWidget } from "./SurveyWidget";

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
    ingestKey,
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
    autoTrackOutboundLinks = true,
    autoTrackFileDownloads = true,
    fileDownloadExtensions,
    autoTrackNotFound = true,
    cookieless = false,
    flags: flagOptions = {},
    surveys: surveyOptions = {},
  } = options;

  const cleanups: InternalCleanup[] = [];

  // Before anything can send an event, so the first page view already goes
  // out without a stored id (ADR 109 D3).
  if (cookieless) enableCookielessIdentity();
  // Session replay needs a visitor that persists across page loads, which is
  // exactly what cookieless mode gives up. Cookieless is page-wide and
  // one-way, so it is read when recording would start, not from this
  // instance's own option: an earlier instance may have switched it on.
  const recordingAllowed = (): boolean => !isCookielessIdentity();

  const enabled = computeEnabled(disabled, ignoreLocalhost);

  const resolveDomain = (): string =>
    domain || (typeof window !== "undefined" ? window.location.hostname : "");

  const sendEvent = async (eventName: string, data: Record<string, JsonValue> = {}): Promise<void> => {
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
      basePath,
      ingestKey
    );
  };

  const trackNotFound: AnalyticsApi["trackNotFound"] = (data = {}) =>
    trackEvent(NOT_FOUND_EVENT, notFoundEventData(data));

  // Created before the first page view so that page view is checked too.
  const notFoundWatcher =
    enabled && autoTrackNotFound
      ? new NotFoundMarkerWatcher(() => void trackNotFound())
      : null;
  if (notFoundWatcher) cleanups.push(() => notFoundWatcher.cancel());

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
      basePath,
      ingestKey
    );
    notFoundWatcher?.afterPageview();
  };

  // Flags work wherever the page is served by Temps, including when
  // analytics is off for localhost; only `disabled` turns them off. Exposure
  // events need analytics, so they are only sent when it is on.
  const flags = new FeatureFlags({
    ...flagOptions,
    basePath,
    disabled,
    onExposure: (exposure) => {
      if (enabled) void sendEvent("flag_exposure", exposureEventData(exposure));
    },
  });

  // Surveys record answers as analytics events, so they need analytics on.
  const surveysEnabled = enabled && surveyOptions.enabled === true;
  const autoShowSurveys = surveysEnabled && surveyOptions.autoShow !== false;
  const surveys = new Surveys({
    basePath,
    key: surveyOptions.key,
    attributes: surveyOptions.attributes,
    onError: surveyOptions.onError,
    disabled: !surveysEnabled,
    onEvent: (name, data) => {
      void sendEvent(name, data);
    },
  });
  let openSurvey: (() => void) | null = null;
  // Set by destroy(): a delivery request still in flight must not open a
  // widget after cleanup has run.
  let destroyed = false;
  const showSurvey = async (event?: string): Promise<void> => {
    if (!autoShowSurveys || openSurvey || destroyed) return;
    await surveys.load();
    if (openSurvey || destroyed) return;
    const next: Survey | undefined = surveys.eligible(window.location.pathname, event)[0];
    if (!next) return;
    surveys.markShown(next);
    // The card stays owned until it has left the page, thank-you message
    // included, so a later event cannot open a second one over it and
    // `destroy()` can still remove it.
    let closed = false;
    const close = renderSurveyWidget(next, {
      onSubmit: (answers) => {
        const result = surveys.submit(next, answers);
        return result.ok ? null : result.errors;
      },
      onDismiss: () => surveys.dismiss(next),
      onClosed: () => {
        closed = true;
        if (openSurvey === closeThis) openSurvey = null;
      },
    });
    const closeThis = (): void => close();
    if (!closed) openSurvey = closeThis;
  };
  cleanups.push(() => {
    destroyed = true;
    openSurvey?.();
  });

  const trackEvent: AnalyticsApi["trackEvent"] = async (eventName, data = {}) => {
    // A survey triggered by this event is shown without waiting for the send.
    if (!eventName.startsWith("survey_")) void showSurvey(eventName);
    await sendEvent(eventName, data);
  };

  const identify: AnalyticsApi["identify"] = async () => {
    // Placeholder for SDK parity; implement when identity endpoint is available.
  };

  if (enabled) {
    if (autoTrackPageviews) {
      cleanups.push(
        setupPageviewTracking(() => {
          trackPageview();
          void showSurvey();
        })
      );
    }

    if (autoTrackEngagement) {
      const tracker = new EngagementTracker({
        basePath,
        ingestKey,
        domain: resolveDomain(),
        heartbeatInterval,
        inactivityTimeout,
        engagementThreshold,
      });
      cleanups.push(() => tracker.destroy());
    } else if (autoTrackPageLeave) {
      cleanups.push(setupLegacyPageLeave(pageLeaveEventName, resolveDomain, basePath, ingestKey));
    }

    if (autoTrackSpeedAnalytics) {
      const speed = new SpeedTracker({ basePath, ingestKey });
      cleanups.push(() => speed.destroy());
    }

    cleanups.push(setupClickDelegation(trackEvent));
    cleanups.push(
      setupLinkTracking(
        { autoTrackOutboundLinks, autoTrackFileDownloads, fileDownloadExtensions },
        (eventName, data) => {
          sendAnalyticsReliable(
            "event",
            {
              event_name: eventName,
              request_query: window.location.search,
              request_path: window.location.pathname,
              domain: resolveDomain(),
              language: navigator.language,
              event_data: data,
            },
            basePath,
            ingestKey
          );
        }
      )
    );

    // Without automatic pageviews, surveys are still offered on load.
    if (!autoTrackPageviews) void showSurvey();
  }

  if (!recordingAllowed() && enableSessionRecording) {
    // eslint-disable-next-line no-console
    console.warn(
      "[temps] Session recording is off in cookieless mode: a replay needs a visitor id stored on the device."
    );
  }
  const recorder =
    enabled && enableSessionRecording && recordingAllowed()
      ? new SessionRecorder({
          basePath,
          ingestKey,
          domain: resolveDomain(),
          enabled: true,
          ...sessionRecordingConfig,
        })
      : null;
  if (recorder) cleanups.push(() => recorder.destroy());

  // Runtime-controlled recorder handle for enable/disable after init
  let runtimeRecorder: SessionRecorder | null = recorder;
  // Another instance switching the page to cookieless ends this one's replay
  // and drops what it has not uploaded yet.
  cleanups.push(
    onCookielessIdentity(() => {
      runtimeRecorder?.discard();
      runtimeRecorder = null;
    })
  );

  return {
    get enabled(): boolean {
      return enabled;
    },
    ingestKey,
    flags,
    surveys,
    trackEvent,
    trackPageview,
    trackNotFound,
    identify,
    enableSessionRecording(): void {
      if (!enabled || !recordingAllowed()) return;
      if (runtimeRecorder) {
        runtimeRecorder.start();
        return;
      }
      runtimeRecorder = new SessionRecorder({
        basePath,
        ingestKey,
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
  basePath: string,
  ingestKey?: string
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
      basePath,
      ingestKey
    );
  };

  window.addEventListener("pagehide", trackPageLeave);
  window.addEventListener("beforeunload", trackPageLeave);

  return (): void => {
    window.removeEventListener("pagehide", trackPageLeave);
    window.removeEventListener("beforeunload", trackPageLeave);
  };
}
