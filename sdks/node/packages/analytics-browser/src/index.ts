// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createAnalytics,
  type AnalyticsApi,
  type AnalyticsOptions,
  type JsonValue,
} from "@temps-sdk/analytics-core";

export * from "@temps-sdk/analytics-core";

type GlobalTemps = AnalyticsApi & {
  trackEvent: AnalyticsApi["trackEvent"];
  trackPageview: AnalyticsApi["trackPageview"];
};

let instance: AnalyticsApi | null = null;

/**
 * Initialize Temps analytics. Safe to call multiple times — subsequent calls
 * replace the previous instance and clean up its listeners.
 */
export function init(options: AnalyticsOptions = {}): AnalyticsApi {
  if (instance) {
    instance.destroy();
  }
  instance = createAnalytics(options);
  if (typeof window !== "undefined") {
    (window as unknown as { temps?: GlobalTemps }).temps = instance as GlobalTemps;
  }
  return instance;
}

/** Returns the current instance, or throws if `init()` was not called. */
export function getAnalytics(): AnalyticsApi {
  if (!instance) {
    throw new Error(
      "Temps analytics has not been initialized. Call init() or use the /auto entrypoint."
    );
  }
  return instance;
}

/** Convenience wrapper — throws if uninitialized. */
export function trackEvent(
  eventName: string,
  data?: Record<string, JsonValue>
): Promise<void> {
  return getAnalytics().trackEvent(eventName, data);
}

/** Convenience wrapper — throws if uninitialized. */
export function trackPageview(): void {
  getAnalytics().trackPageview();
}

/** Tear down the instance and remove listeners. */
export function destroy(): void {
  instance?.destroy();
  instance = null;
  if (typeof window !== "undefined") {
    delete (window as unknown as { temps?: unknown }).temps;
  }
}
