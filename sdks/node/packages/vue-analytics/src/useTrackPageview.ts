// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useTempsAnalytics } from "./useTempsAnalytics";

export function useTrackPageview(): () => void {
  const analytics = useTempsAnalytics();
  return () => analytics.trackPageview();
}
