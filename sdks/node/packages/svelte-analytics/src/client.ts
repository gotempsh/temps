// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { readable, type Readable } from "svelte/store";
import {
  createAnalytics,
  type AnalyticsApi,
  type AnalyticsOptions,
} from "@temps-sdk/analytics-core";

let instance: AnalyticsApi | null = null;

/**
 * Initialize Temps analytics. Call once at app startup (e.g. in your root
 * layout `.svelte` file or a `+layout.ts`).
 *
 * ```ts
 * import { initTempsAnalytics } from "@temps-sdk/svelte-analytics";
 * initTempsAnalytics({ basePath: "/api/_temps" });
 * ```
 */
export function initTempsAnalytics(options: AnalyticsOptions = {}): AnalyticsApi {
  if (instance) instance.destroy();
  instance = createAnalytics(options);
  return instance;
}

/** Returns the current instance, throws if not initialized. */
export function getTempsAnalytics(): AnalyticsApi {
  if (!instance) {
    throw new Error(
      "Temps analytics has not been initialized. Call initTempsAnalytics() first."
    );
  }
  return instance;
}

/**
 * Svelte readable store exposing the analytics instance once initialized.
 * Emits `null` until `initTempsAnalytics()` is called.
 */
export const analyticsStore: Readable<AnalyticsApi | null> = readable<AnalyticsApi | null>(
  null,
  (set) => {
    set(instance);
    const id = setInterval(() => {
      if (instance) {
        set(instance);
        clearInterval(id);
      }
    }, 50);
    return () => clearInterval(id);
  }
);
