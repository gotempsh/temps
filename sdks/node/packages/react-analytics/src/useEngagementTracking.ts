"use client";
import { useEffect, useRef } from "react";
import { EngagementTracker, type EngagementTrackerOptions, type EngagementData } from "./EngagementTracker";
import { useTempsAnalytics } from "./Provider";

export interface UseEngagementTrackingOptions extends Omit<EngagementTrackerOptions, "basePath" | "domain"> {
  /** Whether to enable engagement tracking. Defaults to true. */
  enabled?: boolean;
  /** Callback when engagement data is updated via heartbeat */
  onEngagementUpdate?: (data: EngagementData) => void;
  /** Callback when page leave is triggered */
  onPageLeave?: (data: EngagementData) => void;
}

/**
 * Hook to manually control engagement tracking for specific components or pages.
 * This is useful when you want fine-grained control over engagement tracking
 * or need to track engagement for specific sections of your app.
 *
 * @example
 * ```tsx
 * function ArticlePage() {
 *   const { engagementData } = useEngagementTracking({
 *     heartbeatInterval: 15000, // Send heartbeat every 15 seconds
 *     engagementThreshold: 5000, // Consider engaged after 5 seconds
 *     onEngagementUpdate: (data) => {
 *       console.log('Engagement updated:', data);
 *     }
 *   });
 *
 *   return <article>...</article>;
 * }
 * ```
 */
export function useEngagementTracking(options: UseEngagementTrackingOptions = {}) {
  const analytics = useTempsAnalytics();
  const trackerRef = useRef<EngagementTracker | null>(null);
  const engagementDataRef = useRef<EngagementData>({
    engagement_time_seconds: 0,
    total_time_seconds: 0,
    heartbeat_count: 0,
    is_engaged: false,
    is_visible: true,
    time_since_last_activity: 0,
  });

  const {
    enabled = true,
    onEngagementUpdate,
    onPageLeave,
    ...trackerOptions
  } = options;

  useEffect(() => {
    if (!enabled || !analytics.enabled) {
      return;
    }

    // Create tracker instance
    trackerRef.current = new EngagementTracker({
      ...trackerOptions,
      onHeartbeat: (data) => {
        engagementDataRef.current = data;
        onEngagementUpdate?.(data);
      },
      onPageLeave: (data) => {
        engagementDataRef.current = data;
        onPageLeave?.(data);
      },
    });

    // Cleanup on unmount
    return () => {
      if (trackerRef.current) {
        trackerRef.current.destroy();
        trackerRef.current = null;
      }
    };
  }, [enabled, analytics.enabled]);

  return {
    engagementData: engagementDataRef.current,
    isTracking: Boolean(trackerRef.current),
  };
}
