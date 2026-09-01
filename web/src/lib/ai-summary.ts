// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QuickFilter } from '@/hooks/useAnalyticsDateRange'

export function apiTrafficSummaryCacheKey({
  environmentId,
  filter,
  startIso,
  endIso,
}: {
  environmentId?: number
  filter: QuickFilter
  startIso?: string
  endIso?: string
}): string {
  const environment = environmentId ?? 'all'
  if (filter === 'custom') {
    return `api-traffic:v2:${environment}:custom:${startIso}:${endIso}`
  }
  return `api-traffic:v2:${environment}:${filter}`
}

export function shouldRequestApiTrafficSummary({
  requested,
  dataReady,
  consentEnabled,
}: {
  requested: boolean
  dataReady: boolean
  consentEnabled: boolean
}): boolean {
  return requested && dataReady && consentEnabled
}

export function canStartApiTrafficSummary({
  analyticsEnabled,
  timeseriesPending,
  timeseriesError,
}: {
  analyticsEnabled: boolean
  timeseriesPending: boolean
  timeseriesError: boolean
}): boolean {
  return analyticsEnabled && !timeseriesPending && !timeseriesError
}
