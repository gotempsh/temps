// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { onBeforeUnmount, onMounted, ref } from "vue";
import type { JsonValue } from "@temps-sdk/analytics-core";
import { DEFAULT_BASE_PATH, sendAnalyticsReliable } from "@temps-sdk/analytics-core";
import { useTempsAnalytics } from "./useTempsAnalytics";

export interface UsePageLeaveOptions {
  /** Custom event name. Defaults to "page_leave". */
  eventName?: string;
  /** Additional data to send with the page leave event. */
  eventData?: Record<string, JsonValue>;
  /** Whether to enable page leave tracking. Defaults to true. */
  enabled?: boolean;
  /** Override base path (falls back to the one used by the plugin). */
  basePath?: string;
  /**
   * Analytics ingest key (`pa_…`). This hook sends its unload beacon directly
   * rather than through the plugin instance. Defaults to the key the plugin
   * was initialized with (`useTempsAnalytics().ingestKey`); only pass this to
   * override it for this hook specifically.
   */
  ingestKey?: string;
}

export function usePageLeave(options: UsePageLeaveOptions = {}): {
  triggerPageLeave: () => Promise<void> | void;
} {
  const analytics = useTempsAnalytics();

  const {
    eventName = "page_leave",
    eventData = {},
    enabled = true,
    basePath = DEFAULT_BASE_PATH,
    ingestKey = analytics.ingestKey,
  } = options;
  const hasTracked = ref(false);
  const startTime = ref<number | null>(null);

  const handler = (): void => {
    if (hasTracked.value) return;
    hasTracked.value = true;
    const timeOnPage = startTime.value ? Date.now() - startTime.value : 0;
    sendAnalyticsReliable(
      "event",
      {
        event_name: eventName,
        request_query: window.location.search,
        request_path: window.location.pathname,
        domain: window.location.hostname,
        event_data: {
          ...eventData,
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

  onMounted(() => {
    if (!enabled || !analytics.enabled) return;
    startTime.value = Date.now();
    hasTracked.value = false;
    window.addEventListener("pagehide", handler);
    window.addEventListener("beforeunload", handler);
  });

  onBeforeUnmount(() => {
    window.removeEventListener("pagehide", handler);
    window.removeEventListener("beforeunload", handler);
  });

  const triggerPageLeave = (): Promise<void> | void => {
    if (!enabled || !analytics.enabled || hasTracked.value) return;
    hasTracked.value = true;
    const timeOnPage = startTime.value ? Date.now() - startTime.value : 0;
    return analytics.trackEvent(eventName, {
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
