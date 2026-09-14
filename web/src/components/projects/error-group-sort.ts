// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const EVENT_COUNT_SORT = {
  most: 'events_in_range:desc',
  fewest: 'events_in_range:asc',
} as const

export function errorGroupSortQuery(sort: string) {
  const [sort_by, sort_order] = sort.split(':')
  return { sort_by, sort_order }
}
