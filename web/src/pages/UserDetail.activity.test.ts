// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { summarizeActivity } from './UserDetail.activity'

const entry = (id: number, call_id?: string, succeeded?: boolean) => ({
  id,
  audit_date: id,
  operation_type: 'EXTERNAL_PLUGIN_AI_WRITE',
  data: { call_id, succeeded },
})

test('counts a plugin call once while retaining attempts, outcomes and legacy events', () => {
  expect(
    summarizeActivity(
      [
        entry(1, 'first'),
        entry(2, 'first', true),
        entry(3, 'cancelled'),
        entry(4, 'outcome-only', false),
        entry(5),
        { ...entry(6, 'first'), operation_type: 'LOGIN' },
      ],
      0
    )
  ).toEqual({ actions30d: 5, lastActivity: 6 })
})
test('counts an outcome in the window even when its attempt is outside', () => {
  expect(
    summarizeActivity([entry(1, 'call'), entry(2, 'call', true)], 2)
  ).toEqual({ actions30d: 1, lastActivity: 2 })
  expect(summarizeActivity([], 2)).toEqual({
    actions30d: 0,
    lastActivity: undefined,
  })
})
