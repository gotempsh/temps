// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  observationError,
  patchObservationFilters,
  positiveInteger,
  readObservationWindow,
} from './global-observability'

const now = Date.parse('2026-09-09T12:00:00Z')
describe('global observability URL state', () => {
  test('normalizes invalid windows without sending unbounded queries', () => {
    const window = readObservationWindow(
      new URLSearchParams('range=forever&from=invalid&to=invalid'),
      now
    )
    expect(window).toEqual({
      range: '1d',
      from: '2026-09-08T12:00:00.000Z',
      to: '2026-09-09T12:00:00.000Z',
    })
    const oversized = readObservationWindow(
      new URLSearchParams('from=2020-01-01&to=2026-01-01'),
      now
    )
    expect(oversized).toEqual(window)
  })
  test('reloading and paging preserves the exact window a log cursor was issued for', () => {
    const params = new URLSearchParams(
      'from=2026-09-08T12:00:00Z&to=2026-09-09T12:00:00Z&cursor=opaque-token'
    )
    expect(readObservationWindow(params, now + 60000)).toEqual(
      readObservationWindow(params, now)
    )
  })
  test('changing any filter drops pagination tokens but preserves scope and time', () => {
    const current = new URLSearchParams(
      'project_id=4&page=5&cursor=old&q=before&from=start&to=end'
    )
    const next = patchObservationFilters(current, {
      q: 'after',
      level: 'ERROR',
    })
    expect(next.has('page')).toBe(false)
    expect(next.has('cursor')).toBe(false)
    expect(next.get('project_id')).toBe('4')
    expect(next.get('from')).toBe('start')
    expect(next.get('q')).toBe('after')
    expect(current.get('cursor')).toBe('old')
  })
  test('rejects invalid scope IDs and pages', () => {
    for (const value of ['-1', '0', '1.2', 'abc', '9007199254740992'])
      expect(positiveInteger(value)).toBeUndefined()
    expect(positiveInteger('12')).toBe(12)
  })
  test('uses backend permission and saturation explanations', () => {
    expect(observationError({ title: 'Access denied' })).toBe('Access denied')
    expect(
      observationError({
        title: 'Busy',
        detail: 'Two searches are already running',
      })
    ).toBe('Two searches are already running')
  })
})

test('custom windows and legacy one-day links retain their meaning', () => {
  const custom = new URLSearchParams(
    'range=custom&from=2026-09-08T09:30:00Z&to=2026-09-09T11:45:00Z'
  )
  expect(readObservationWindow(custom, now)).toEqual({
    range: 'custom',
    from: '2026-09-08T09:30:00.000Z',
    to: '2026-09-09T11:45:00.000Z',
  })
  expect(
    readObservationWindow(new URLSearchParams('range=24h'), now).range
  ).toBe('1d')
  expect(
    readObservationWindow(new URLSearchParams('range=custom&from=invalid'), now)
      .range
  ).toBe('1d')
})
