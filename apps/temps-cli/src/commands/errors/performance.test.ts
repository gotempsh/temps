// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { checkIssueType } from './performance.js'

describe('checkIssueType', () => {
  test('accepts the two types in any case', () => {
    expect(checkIssueType(' Performance ')).toBe('performance')
    expect(checkIssueType('error')).toBe('error')
  })

  test('names the types when one is unknown', () => {
    expect(() => checkIssueType('crash')).toThrow(/error or performance/)
  })
})
