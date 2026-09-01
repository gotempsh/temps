// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useCallback } from "react";
import { useTempsAnalytics } from "./Provider";

export function useTrackPageview() {
  const { trackPageview } = useTempsAnalytics();
  return useCallback(() => trackPageview(), [trackPageview]);
}
