// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { parseHosting } from './hosting.js'

describe('parseHosting', () => {
  test('accepts hosted, external and elsewhere', () => {
    expect(parseHosting('hosted')).toBe('hosted')
    expect(parseHosting('External')).toBe('external')
    expect(parseHosting('elsewhere')).toBe('external')
    expect(() => parseHosting('vercel')).toThrow(/hosted or external/)
  })
})
