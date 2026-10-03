// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
export interface UseEngagementTrackingResult {
  engagementData: EngagementData;
  isTracking: boolean;
}

export function useEngagementTracking(
  options: UseEngagementTrackingOptions = {}
): UseEngagementTrackingResult {
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

  const { enabled = true } = options;

  // Callers usually pass an inline options object, so it is a new value on
  // every render. Keeping it in a ref lets the callbacks always see the latest
  // closure without tearing the tracker down and recreating it each render.
  // Tracker settings (intervals, thresholds) are read when tracking starts.
  const optionsRef = useRef(options);
  optionsRef.current = options;

  useEffect(() => {
    if (!enabled || !analytics.enabled) {
      return;
    }

    const {
      enabled: _enabled,
      onEngagementUpdate: _onEngagementUpdate,
      onPageLeave: _onPageLeave,
      ...trackerOptions
    } = optionsRef.current;

    // Create tracker instance
    trackerRef.current = new EngagementTracker({
      ...trackerOptions,
      onHeartbeat: (data) => {
        engagementDataRef.current = data;
        optionsRef.current.onEngagementUpdate?.(data);
      },
      onPageLeave: (data) => {
        engagementDataRef.current = data;
        optionsRef.current.onPageLeave?.(data);
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
