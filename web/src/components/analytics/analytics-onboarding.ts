// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type AnalyticsOnboardingFeature =
  'live-visitors' | 'live-globe' | 'journey'

export interface AnalyticsOnboardingCopy {
  /** Name of the view, as it appears in the analytics navigation. */
  title: string
  /** What the view shows once events arrive, with a concrete example. */
  example: string
}

export const ANALYTICS_ONBOARDING_COPY: Record<
  AnalyticsOnboardingFeature,
  AnalyticsOnboardingCopy
> = {
  'live-visitors': {
    title: 'Live visitors',
    example:
      'Shows who is on your site right now, for example a visitor from Lisbon reading /pricing in Firefox, updated every few seconds.',
  },
  'live-globe': {
    title: 'Live globe',
    example:
      'Plots visitors on a globe as they arrive and streams their page views and events, for example "Berlin viewed /docs/getting-started".',
  },
  journey: {
    title: 'Journey',
    example:
      'Shows how visitors move between pages, for example / → /pricing → /signup, and where they drop off.',
  },
}

export type AnalyticsInstallState = 'checking' | 'not-installed' | 'installed'

/**
 * Resolve whether a project has analytics installed from the has-events
 * check. A failed check is treated as installed so the view still renders
 * (and reports its own errors) instead of hiding behind a setup prompt that
 * may be wrong.
 */
export function resolveAnalyticsInstallState(query: {
  isPending: boolean
  isError: boolean
  data?: { has_events: boolean }
}): AnalyticsInstallState {
  if (query.isError) return 'installed'
  if (query.isPending || !query.data) return 'checking'
  return query.data.has_events ? 'installed' : 'not-installed'
}

/** Accessible label for the project header's live visitors pill. */
export function liveVisitorsPillLabel(activeVisitors: number): string {
  if (activeVisitors <= 0)
    return 'No active visitors right now. Open Live visitors'
  return `${activeVisitors} active visitor${activeVisitors === 1 ? '' : 's'}. Open Live visitors`
}
