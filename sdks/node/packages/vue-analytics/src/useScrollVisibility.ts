// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { onBeforeUnmount, ref, watch, type Ref } from "vue";
import type { JsonValue } from "@temps-sdk/analytics-core";
import { useTempsAnalytics } from "./useTempsAnalytics";

export interface UseScrollVisibilityOptions {
  eventName?: string;
  eventData?: Record<string, JsonValue>;
  threshold?: number;
  root?: Element | null;
  rootMargin?: string;
  once?: boolean;
  enabled?: boolean;
}

/**
 * Tracks when an element scrolls into view using IntersectionObserver.
 * Returns a `Ref<HTMLElement | null>` to bind with `ref="elementRef"` in a template.
 */
export function useScrollVisibility(
  options: UseScrollVisibilityOptions = {}
): Ref<HTMLElement | null> {
  const {
    eventName = "component_visible",
    eventData,
    threshold = 0.5,
    root = null,
    rootMargin = "0px",
    once = true,
    enabled = true,
  } = options;

  const analytics = useTempsAnalytics();
  const elementRef = ref<HTMLElement | null>(null);
  const hasTracked = ref(false);
  let observer: IntersectionObserver | null = null;

  const cleanup = (): void => {
    observer?.disconnect();
    observer = null;
  };

  watch(
    elementRef,
    (node) => {
      cleanup();
      if (!enabled || !node || typeof IntersectionObserver === "undefined") return;
      if (!once) hasTracked.value = false;

      observer = new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            if (entry.isIntersecting && (!once || !hasTracked.value)) {
              void analytics.trackEvent(eventName, eventData);
              hasTracked.value = true;
            }
          });
        },
        { root, rootMargin, threshold }
      );
      observer.observe(node);
    },
    { immediate: true, flush: "post" }
  );

  onBeforeUnmount(cleanup);

  return elementRef;
}
