// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { scheduleOptions } from './schedule-options'

describe('backup schedule presets', () => {
  test('uses an unambiguous Sunday value for the weekly preset', () => {
    const weekly = scheduleOptions.find((option) => option.label === 'Weekly')

    expect(weekly?.value).toBe('0 0 0 * * SUN')
  })
})
