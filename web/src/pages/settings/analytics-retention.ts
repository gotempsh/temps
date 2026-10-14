// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AnalyticsRetentionSettings } from '@/api/platformSettings'

export const ANALYTICS_RETENTION_MIN_DAYS = 1
export const ANALYTICS_RETENTION_MAX_DAYS = 3650

export const ANALYTICS_RETENTION_DEFAULTS: AnalyticsRetentionSettings = {
  events_days: 90,
  error_events_days: null,
  visitor_days: null,
  unreferenced_ip_days: 30,
}

export interface AnalyticsRetentionField {
  name: keyof AnalyticsRetentionSettings
  label: string
  /** What is stored, so the admin knows which data the window governs. */
  stores: string
  description: string
  /** A field that can be left empty to keep the data forever. */
  optional: boolean
}

export const ANALYTICS_RETENTION_FIELDS: AnalyticsRetentionField[] = [
  {
    name: 'events_days',
    label: 'Page views and events',
    stores: 'events',
    description:
      'Raw page views and custom events, including the IP address each one was recorded from. Applies to the built-in database: when ClickHouse analytics is enabled, its copy of events keeps to the ClickHouse table TTL (5 years by default), which this window does not change.',
    optional: false,
  },
  {
    name: 'error_events_days',
    label: 'Error events',
    stores: 'error_events',
    description:
      'Individual error occurrences. Error groups and their counters are kept.',
    optional: true,
  },
  {
    name: 'visitor_days',
    label: 'Visitors',
    stores: 'visitor',
    description:
      'Visitors not seen for this long are deleted and no longer appear in visitor lists. Deleting a visitor also deletes its session replays, and detaches it from the events, error events and request logs that are kept.',
    optional: true,
  },
  {
    name: 'unreferenced_ip_days',
    label: 'Stored IP addresses',
    stores: 'ip_geolocations',
    description:
      'A stored address is deleted once no event, visitor, log or audit entry uses it any more and it has not been looked up for this long.',
    optional: true,
  },
]

/**
 * A cleared number input yields `''`, which must become `null` ("use the
 * default") rather than `NaN` — the server rejects a non-number and would
 * otherwise report a validation error for a field the admin just cleared.
 */
export function emptyToNull(value: unknown): number | null {
  if (value === '' || value === null || value === undefined) return null
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : null
}

/**
 * Validation for one analytics retention window. `null` is "keep" (or "not
 * set"): allowed for a window that can be left empty, a message otherwise.
 */
export function validateRetentionWindow(
  value: unknown,
  optional: boolean
): true | string {
  if (value === null) {
    return optional || 'A retention window is required'
  }
  return (
    (typeof value === 'number' &&
      Number.isInteger(value) &&
      value >= ANALYTICS_RETENTION_MIN_DAYS &&
      value <= ANALYTICS_RETENTION_MAX_DAYS) ||
    'Must be a whole number of days from 1 to 3650'
  )
}
