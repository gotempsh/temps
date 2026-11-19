// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  IGNORE_PRESETS,
  describeIgnoreState,
  ignorePresetConditions,
} from './ignore-presets'

describe('ignorePresetConditions', () => {
  const now = new Date('2026-01-10T12:00:00.000Z')

  test('a plain ignore has no conditions', () => {
    expect(ignorePresetConditions('next_event', now)).toBeUndefined()
  })

  test('time presets resolve to an absolute time', () => {
    expect(ignorePresetConditions('one_hour', now)).toEqual({
      until: '2026-01-10T13:00:00.000Z',
    })
    expect(ignorePresetConditions('one_week', now)).toEqual({
      until: '2026-01-17T12:00:00.000Z',
    })
  })

  test('count, user and forever presets', () => {
    expect(ignorePresetConditions('hundred_more', now)).toEqual({ event_count: 100 })
    expect(ignorePresetConditions('ten_users', now)).toEqual({ user_count: 10 })
    expect(ignorePresetConditions('forever', now)).toEqual({ forever: true })
  })

  test('every preset has a label', () => {
    expect(IGNORE_PRESETS.map((p) => p.preset)).toHaveLength(7)
  })
})

describe('describeIgnoreState', () => {
  const date = () => 'Jan 17'

  test('forever', () => {
    expect(describeIgnoreState({ user_count: 0, forever: true }, 5)).toContain('forever')
  })

  test('combines the conditions and shows progress', () => {
    expect(
      describeIgnoreState(
        {
          until: '2026-01-17T12:00:00Z',
          until_event_count: 105,
          until_user_count: 10,
          user_count: 3,
          forever: false,
        },
        100,
        date
      )
    ).toBe(
      'Ignored until Jan 17, or for 5 more occurrences, or until 10 new users are affected (3 so far), whichever comes first.'
    )
  })
})
