// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useEffect, useRef } from "react";
import { useTempsAnalytics } from "./Provider";
import { sendAnalyticsReliable } from "./utils";
import { DEFAULT_BASE_PATH } from "./constants";
import type { JsonValue } from "./types";

export interface UsePageLeaveOptions {
  /** Custom event name. Defaults to "page_leave" */
  eventName?: string;
  /** Additional data to send with the page leave event */
  eventData?: Record<string, JsonValue>;
  /** Whether to enable page leave tracking. Defaults to true */
  enabled?: boolean;
}

export interface UsePageLeaveResult {
  /** Send the page-leave event now; resolves once sent, `undefined` if skipped. */
  triggerPageLeave: () => Promise<void> | undefined;
}

export function usePageLeave(options: UsePageLeaveOptions = {}): UsePageLeaveResult {
  const {
    eventName = "page_leave",
    eventData = {},
    enabled = true
  } = options;

  const { enabled: analyticsEnabled, trackEvent } = useTempsAnalytics();
  const hasTrackedRef = useRef(false);
  const startTimeRef = useRef<number>();

  useEffect(() => {
    startTimeRef.current = Date.now();
    hasTrackedRef.current = false;
  }, []);

  useEffect(() => {
    if (!enabled || !analyticsEnabled) return;

    const trackPageLeave = (): void => {
      if (hasTrackedRef.current) return;
      hasTrackedRef.current = true;

      const timeOnPage = startTimeRef.current ? Date.now() - startTimeRef.current : 0;

      const finalEventData = {
        ...eventData,
        time_on_page_ms: timeOnPage,
        timestamp: new Date().toISOString(),
        url: window.location.href,
        referrer: document.referrer,
      };

      // Use reliable sending method that tries sendBeacon first
      sendAnalyticsReliable("event", {
        event_name: eventName,
        request_query: window.location.search,
        request_path: window.location.pathname,
        domain: window.location.hostname,
        event_data: finalEventData,
      }, DEFAULT_BASE_PATH);
    };

    // Use pagehide as primary (most reliable), with beforeunload as fallback
    const handlePageLeave = (): void => trackPageLeave();

    // pagehide is the most reliable for modern browsers
    window.addEventListener("pagehide", handlePageLeave);
    // beforeunload as fallback for older browsers
    window.addEventListener("beforeunload", handlePageLeave);

    return () => {
      window.removeEventListener("pagehide", handlePageLeave);
      window.removeEventListener("beforeunload", handlePageLeave);
    };
  }, [enabled, analyticsEnabled, eventName, eventData]);

  // Manual trigger function
  const triggerPageLeave = (): Promise<void> | undefined => {
    if (!enabled || !analyticsEnabled || hasTrackedRef.current) return;

    hasTrackedRef.current = true;
    const timeOnPage = startTimeRef.current ? Date.now() - startTimeRef.current : 0;

    return trackEvent(eventName, {
      ...eventData,
      time_on_page_ms: timeOnPage,
      timestamp: new Date().toISOString(),
      url: window.location.href,
      referrer: document.referrer,
      manual_trigger: true,
    });
  };

  return { triggerPageLeave };
}
