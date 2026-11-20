// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useCallback, useSyncExternalStore } from "react";
import type { FeatureFlags, FlagContext } from "@temps-sdk/analytics-core";
import { useTempsAnalytics } from "./Provider";

function useFlagsVersion(flags: FeatureFlags): number {
  const subscribe = useCallback((listener: () => void) => flags.subscribe(listener), [flags]);
  const version = useCallback(() => flags.version, [flags]);
  return useSyncExternalStore(subscribe, version, version);
}

/**
 * The value Temps serves for a client-visible flag, for this visitor.
 *
 * Returns `fallback` until flags arrive (or if the flag does not exist, is
 * not client-visible, or the request fails), then re-renders with the served
 * value. Reading a flag that records exposures sends one `flag_exposure`
 * event per page.
 *
 * ```tsx
 * const newCheckout = useFlag("checkout.v2", false);
 * ```
 */
export function useFlag<T>(key: string, fallback: T): T {
  const { flags } = useTempsAnalytics();
  useFlagsVersion(flags);
  return flags.get(key, fallback);
}

/** The variant served for a flag (`"control"`, `"test"`, ...), or `null`. */
export function useFlagVariant(key: string): string | null {
  const { flags } = useTempsAnalytics();
  useFlagsVersion(flags);
  return flags.getVariant(key);
}

/**
 * The flags client, for re-evaluating with a new context after sign-in:
 *
 * ```tsx
 * const { reload } = useFeatureFlags();
 * useEffect(() => { if (user) void reload({ key: user.id, attributes: { plan: user.plan } }); }, [user]);
 * ```
 */
export function useFeatureFlags(): {
  flags: FeatureFlags;
  status: FeatureFlags["status"];
  reload: (context?: FlagContext) => Promise<void>;
} {
  const { flags } = useTempsAnalytics();
  useFlagsVersion(flags);
  const reload = useCallback((context?: FlagContext) => flags.reload(context), [flags]);
  return { flags, status: flags.status, reload };
}
