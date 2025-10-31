"use client";
import { useCallback } from "react";
import { useTempsAnalytics } from "./Provider";

export function useTrackPageview() {
  const { trackPageview } = useTempsAnalytics();
  return useCallback(() => trackPageview(), [trackPageview]);
}
