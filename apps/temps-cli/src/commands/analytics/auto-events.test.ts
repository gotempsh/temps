// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MAX_NOT_FOUND_WINDOW_DAYS,
  clampNotFoundStart,
  notFoundRequest,
  parseLimit,
} from './auto-events.js'

describe('parseLimit', () => {
  test('defaults to 20 and accepts 1..100', () => {
    expect(parseLimit(undefined)).toBe(20)
    expect(parseLimit('1')).toBe(1)
    expect(parseLimit('100')).toBe(100)
  })

  test('refuses values the API would reject or silently clamp', () => {
    for (const raw of ['0', '101', '2.5', 'ten', '']) {
      expect(() => parseLimit(raw)).toThrow(/--limit must be a whole number/)
    }
  })
})

describe('clampNotFoundStart', () => {
  const end = '2026-11-19T00:00:00.000Z'

  test('keeps a window within the aggregation limit', () => {
    expect(clampNotFoundStart('2026-11-12T00:00:00.000Z', end)).toEqual({
      startDate: '2026-11-12T00:00:00.000Z',
      narrowed: false,
    })
  })

  test('narrows a longer window to its most recent days', () => {
    const result = clampNotFoundStart('2026-08-01T00:00:00.000Z', end)
    expect(result.narrowed).toBe(true)
    expect(new Date(end).getTime() - new Date(result.startDate).getTime()).toBe(
      MAX_NOT_FOUND_WINDOW_DAYS * 86_400_000,
    )
  })
})

describe('notFoundRequest', () => {
  test('ranks 404 paths answered to visitors that are not bots', () => {
    const request = notFoundRequest('2026-11-12T00:00:00Z', '2026-11-19T00:00:00Z', 15)
    expect(request.dimensions).toEqual(['path'])
    expect(request.page_size).toBe(15)
    expect(request.filters).toEqual([
      { dimension: 'status_code', operator: 'eq', values: ['404'] },
      { dimension: 'is_bot', operator: 'eq', values: ['false'] },
    ])
  })
})
