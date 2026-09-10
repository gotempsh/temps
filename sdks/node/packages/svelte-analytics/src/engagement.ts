// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { readable, type Readable } from "svelte/store";
import {
  EngagementTracker,
  type EngagementData,
  type EngagementTrackerOptions,
} from "@temps-sdk/analytics-core";

export type EngagementStore = Readable<EngagementData>;

const EMPTY: EngagementData = {
  engagement_time_seconds: 0,
  total_time_seconds: 0,
  heartbeat_count: 0,
  is_engaged: false,
  is_visible: true,
  time_since_last_activity: 0,
};

/**
 * Returns a readable store that emits engagement data on every heartbeat.
 * The underlying tracker is created on first subscription and destroyed when
 * the last subscriber unsubscribes.
 */
export function engagementStore(
  options: Omit<EngagementTrackerOptions, "onHeartbeat" | "onPageLeave"> = {}
): EngagementStore {
  return readable<EngagementData>(EMPTY, (set) => {
    if (typeof window === "undefined") return;
    const tracker = new EngagementTracker({
      ...options,
      onHeartbeat: (data) => set(data),
      onPageLeave: (data) => set(data),
    });
    return () => tracker.destroy();
  });
}
