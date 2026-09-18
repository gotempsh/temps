// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type EventKind } from './types'

export const TIME_RANGES = [
  { value: '15m', label: 'Last 15 minutes' },
  { value: '1h', label: 'Last hour' },
  { value: '24h', label: 'Last 24 hours' },
  { value: '7d', label: 'Last 7 days' },
  { value: '30d', label: 'Last 30 days' },
] as const

export type TimeRange = string

export interface ObserveFilters {
  kinds: EventKind[]
  timeRange: TimeRange
  search: string
  environmentId: number | null
  hideBots: boolean
}
