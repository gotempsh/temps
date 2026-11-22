// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  replayCustomDescription,
  replayEventData,
} from './replay-event-description'

test('unwraps complete rrweb events without changing recording data', () => {
  const row = {
    type: 3,
    timestamp: 1790000000000,
    data: { source: 5, text: '***' },
  }
  expect(replayEventData(row)).toEqual({ source: 5, text: '***' })
  expect(row.type).toBe(3)
  expect(replayEventData(null)).toEqual({})
})
test('distinguishes technical replay events from custom application events', () => {
  for (const [tag, payload, label] of [
    ['sentry.replay', {}, 'Replay segment metadata'],
    ['performanceSpan', {}, 'Performance measurement'],
    ['breadcrumb', { category: 'console' }, 'Console breadcrumb'],
    ['breadcrumb', { category: 'ui.click' }, 'Interaction breadcrumb'],
    ['breadcrumb', { category: 'fetch' }, 'Network breadcrumb'],
    ['synthetic.checkout', {}, 'Custom event: synthetic.checkout'],
  ] as const) {
    expect(replayCustomDescription({ type: 5, data: { tag, payload } })).toBe(
      label
    )
  }
})
