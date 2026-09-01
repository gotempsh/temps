// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Action } from "svelte/action";
import type { JsonValue } from "@temps-sdk/analytics-core";
import { getTempsAnalytics } from "./client";

export interface TrackVisibilityOptions {
  eventName?: string;
  eventData?: Record<string, JsonValue>;
  threshold?: number;
  root?: Element | null;
  rootMargin?: string;
  once?: boolean;
  enabled?: boolean;
}

/**
 * Svelte action that fires an analytics event the first time (or every time,
 * if `once: false`) a node scrolls into view.
 *
 * ```svelte
 * <section use:trackVisibility={{ eventName: "pricing_viewed" }}>
 *   Pricing
 * </section>
 * ```
 */
export const trackVisibility: Action<HTMLElement, TrackVisibilityOptions | undefined> = (
  node,
  params
) => {
  let opts: TrackVisibilityOptions = params ?? {};
  let hasTracked = false;
  let observer: IntersectionObserver | null = null;

  const start = (): void => {
    if (opts.enabled === false) return;
    if (typeof IntersectionObserver === "undefined") return;

    const {
      eventName = "component_visible",
      eventData,
      threshold = 0.5,
      root = null,
      rootMargin = "0px",
      once = true,
    } = opts;

    observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (entry.isIntersecting && (!once || !hasTracked)) {
            void getTempsAnalytics().trackEvent(eventName, eventData);
            hasTracked = true;
          }
        });
      },
      { root, rootMargin, threshold }
    );
    observer.observe(node);
  };

  const stop = (): void => {
    observer?.disconnect();
    observer = null;
  };

  start();

  return {
    update(next): void {
      opts = next ?? {};
      hasTracked = false;
      stop();
      start();
    },
    destroy(): void {
      stop();
    },
  };
};
