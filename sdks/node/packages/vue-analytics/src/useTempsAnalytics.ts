// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { inject } from "vue";
import type { AnalyticsApi } from "@temps-sdk/analytics-core";
import { TempsAnalyticsKey } from "./plugin";

export function useTempsAnalytics(): AnalyticsApi {
  const analytics = inject(TempsAnalyticsKey);
  if (!analytics) {
    throw new Error(
      "useTempsAnalytics() requires app.use(TempsAnalyticsPlugin) at the root of your app."
    );
  }
  return analytics;
}
