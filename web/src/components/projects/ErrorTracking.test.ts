// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { errorGroupSortQuery, EVENT_COUNT_SORT } from './error-group-sort'

test('event sorting requests the count displayed for the selected date range', () => {
  expect(errorGroupSortQuery(EVENT_COUNT_SORT.most)).toEqual({
    sort_by: 'events_in_range',
    sort_order: 'desc',
  })
  expect(errorGroupSortQuery(EVENT_COUNT_SORT.fewest)).toEqual({
    sort_by: 'events_in_range',
    sort_order: 'asc',
  })
})
