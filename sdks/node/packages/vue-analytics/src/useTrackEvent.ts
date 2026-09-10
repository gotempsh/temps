// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { JsonValue } from "@temps-sdk/analytics-core";
import { useTempsAnalytics } from "./useTempsAnalytics";

export function useTrackEvent(): (
  eventName: string,
  data?: Record<string, JsonValue>
) => Promise<void> {
  const analytics = useTempsAnalytics();
  return (eventName, data) => analytics.trackEvent(eventName, data);
}
