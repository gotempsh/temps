// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { parseHealthCommand } from './health-probe'

describe('health command arguments', () => {
  test('preserves arguments without shell expansion', () => {
    expect(
      parseHealthCommand(
        '["check-worker", "argument with spaces", "$VALUE", ""]'
      )
    ).toEqual(['check-worker', 'argument with spaces', '$VALUE', ''])
  })
  test('rejects missing executables and malformed commands', () => {
    for (const value of [
      '',
      'check-worker --ready',
      '[]',
      '[""]',
      '[" "]',
      '["check", 1]',
      'null',
      '{}',
    ]) {
      expect(parseHealthCommand(value)).toBeNull()
    }
  })
})
