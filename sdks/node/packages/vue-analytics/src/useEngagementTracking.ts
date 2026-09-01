// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { onBeforeUnmount, onMounted, ref, type Ref } from "vue";
import {
  EngagementTracker,
  type EngagementData,
  type EngagementTrackerOptions,
} from "@temps-sdk/analytics-core";
import { useTempsAnalytics } from "./useTempsAnalytics";

export interface UseEngagementTrackingOptions
  extends Omit<EngagementTrackerOptions, "basePath" | "domain"> {
  enabled?: boolean;
  onEngagementUpdate?: (data: EngagementData) => void;
  onPageLeave?: (data: EngagementData) => void;
}

/**
 * Manual engagement tracking for specific routes / components.
 * Mirrors the React `useEngagementTracking` hook.
 */
export function useEngagementTracking(
  options: UseEngagementTrackingOptions = {}
): {
  engagementData: Ref<EngagementData>;
  isTracking: Ref<boolean>;
} {
  const analytics = useTempsAnalytics();
  const tracker = ref<EngagementTracker | null>(null);
  const isTracking = ref(false);
  const engagementData = ref<EngagementData>({
    engagement_time_seconds: 0,
    total_time_seconds: 0,
    heartbeat_count: 0,
    is_engaged: false,
    is_visible: true,
    time_since_last_activity: 0,
  });

  const { enabled = true, onEngagementUpdate, onPageLeave, ...trackerOptions } = options;

  onMounted(() => {
    if (!enabled || !analytics.enabled) return;
    tracker.value = new EngagementTracker({
      ...trackerOptions,
      onHeartbeat: (data) => {
        engagementData.value = data;
        onEngagementUpdate?.(data);
      },
      onPageLeave: (data) => {
        engagementData.value = data;
        onPageLeave?.(data);
      },
    });
    isTracking.value = true;
  });

  onBeforeUnmount(() => {
    tracker.value?.destroy();
    tracker.value = null;
    isTracking.value = false;
  });

  return { engagementData, isTracking };
}
