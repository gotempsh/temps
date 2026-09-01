// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useCallback } from "react";
import { useTempsAnalytics } from "./Provider";
import type { JsonValue } from "./types";

export function useTrackEvent() {
  const { trackEvent } = useTempsAnalytics();
  return useCallback((eventName: string, data?: Record<string, JsonValue>) => trackEvent(eventName, data), [trackEvent]);
}
