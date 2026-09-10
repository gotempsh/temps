// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export * from "@temps-sdk/analytics-core";
export { TempsAnalyticsPlugin, TempsAnalyticsKey } from "./plugin";
export { useTempsAnalytics } from "./useTempsAnalytics";
export { useTrackEvent } from "./useTrackEvent";
export { useTrackPageview } from "./useTrackPageview";
export { usePageLeave, type UsePageLeaveOptions } from "./usePageLeave";
export {
  useEngagementTracking,
  type UseEngagementTrackingOptions,
} from "./useEngagementTracking";
export {
  useScrollVisibility,
  type UseScrollVisibilityOptions,
} from "./useScrollVisibility";
export {
  useSessionRecording,
  type UseSessionRecordingReturn,
} from "./useSessionRecording";
