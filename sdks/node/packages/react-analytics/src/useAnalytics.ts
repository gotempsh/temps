"use client";
import { useCallback, useMemo } from "react";

export type AnalyticsEventPayload = Record<string, unknown>;

export interface AnalyticsClient {
  track(eventName: string, payload?: AnalyticsEventPayload): void | Promise<void>;
  identify?(userId: string, traits?: Record<string, unknown>): void | Promise<void>;
}

export interface UseAnalyticsOptions {
  client: AnalyticsClient;
  defaultContext?: Record<string, unknown>;
}

export function useAnalytics(options: UseAnalyticsOptions) {
  const { client, defaultContext } = options;

  const track = useCallback(
    (eventName: string, payload?: AnalyticsEventPayload) => {
      const finalPayload = defaultContext ? { ...defaultContext, ...payload } : payload;
      return client.track(eventName, finalPayload);
    },
    [client, defaultContext]
  );

  const identify = useCallback(
    (userId: string, traits?: Record<string, unknown>) => {
      if (!client.identify) return;
      return client.identify(userId, traits);
    },
    [client]
  );

  return useMemo(() => ({ track, identify }), [track, identify]);
}
