// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MAX_LINES_PER_SEC,
  budgetInputValue,
  describeBudget,
  parseBudget,
} from './log-ingestion-budget'

describe('parseBudget', () => {
  test('blank inherits, 0 lifts, in-range numbers override', () => {
    expect(parseBudget('  ', MAX_LINES_PER_SEC)).toBeNull()
    expect(parseBudget('0', MAX_LINES_PER_SEC)).toBe(0)
    expect(parseBudget('2000', MAX_LINES_PER_SEC)).toBe(2000)
    expect(parseBudget(String(MAX_LINES_PER_SEC), MAX_LINES_PER_SEC)).toBe(
      MAX_LINES_PER_SEC
    )
  })

  test('negative, fractional, too large and non-numeric are invalid', () => {
    for (const raw of ['-1', '1.5', String(MAX_LINES_PER_SEC + 1), 'abc']) {
      expect(parseBudget(raw, MAX_LINES_PER_SEC)).toBeUndefined()
    }
  })
})

test('budgetInputValue shows null as blank', () => {
  expect(budgetInputValue(null)).toBe('')
  expect(budgetInputValue(undefined)).toBe('')
  expect(budgetInputValue(0)).toBe('0')
})

test('describeBudget names the platform default, unlimited and own budget', () => {
  expect(describeBudget(null, 500, 'lines/s')).toBe(
    'Platform default: 500 lines/s'
  )
  expect(describeBudget(null, 0, 'KiB/s')).toBe('Platform default: unlimited')
  expect(describeBudget(undefined, undefined, 'KiB/s')).toBe('Platform default')
  expect(describeBudget(0, 500, 'lines/s')).toBe('Unlimited')
  expect(describeBudget(2000, 500, 'lines/s')).toBe(
    '2,000 lines/s (own budget)'
  )
})
