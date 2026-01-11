"use client";
import { useCallback, useEffect, useRef } from "react";
import { useTempsAnalytics } from "./Provider";
import type { JsonValue } from "./types";

export interface UseScrollVisibilityOptions {
  /**
   * Event name to track when component becomes visible.
   * @default "component_visible"
   */
  eventName?: string;

  /**
   * Additional data to send with the event.
   */
  eventData?: Record<string, JsonValue>;

  /**
   * Percentage of the element that must be visible (0.0 to 1.0).
   * @default 0.5
   */
  threshold?: number;

  /**
   * Root element for intersection observer (null = viewport).
   * @default null
   */
  root?: Element | null;

  /**
   * Margin around root element.
   * @default "0px"
   */
  rootMargin?: string;

  /**
   * Whether to track only once (true) or every time it becomes visible (false).
   * @default true
   */
  once?: boolean;

  /**
   * Whether tracking is enabled.
   * @default true
   */
  enabled?: boolean;
}

/**
 * Hook that tracks when a component scrolls into view using Intersection Observer.
 *
 * @example
 * ```tsx
 * function MyComponent() {
 *   const ref = useScrollVisibility({
 *     eventName: "hero_section_viewed",
 *     eventData: { section: "hero" },
 *     threshold: 0.75
 *   });
 *
 *   return <div ref={ref}>Hero Section</div>;
 * }
 * ```
 *
 * @param options - Configuration options
 * @returns A ref callback to attach to the element you want to track
 */
export function useScrollVisibility(options: UseScrollVisibilityOptions = {}) {
  const {
    eventName = "component_visible",
    eventData,
    threshold = 0.5,
    root = null,
    rootMargin = "0px",
    once = true,
    enabled = true,
  } = options;

  const { trackEvent } = useTempsAnalytics();
  const hasTrackedRef = useRef(false);
  const observerRef = useRef<IntersectionObserver | null>(null);

  // Cleanup function
  const cleanup = useCallback(() => {
    if (observerRef.current) {
      observerRef.current.disconnect();
      observerRef.current = null;
    }
  }, []);

  // Callback ref that sets up the observer when element is attached
  const ref = useCallback(
    (node: HTMLElement | null) => {
      // Clean up previous observer
      cleanup();

      if (!enabled || !node) {
        return;
      }

      // Reset tracking state if once is false
      if (!once) {
        hasTrackedRef.current = false;
      }

      observerRef.current = new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            if (entry.isIntersecting && (!once || !hasTrackedRef.current)) {
              trackEvent(eventName, eventData);
              hasTrackedRef.current = true;
            }
          });
        },
        {
          root,
          rootMargin,
          threshold,
        }
      );

      observerRef.current.observe(node);
    },
    [eventName, eventData, threshold, root, rootMargin, once, enabled, trackEvent, cleanup]
  );

  // Cleanup on unmount
  useEffect(() => {
    return cleanup;
  }, [cleanup]);

  return ref;
}
