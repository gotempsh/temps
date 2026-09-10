// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { selectTargetEnvironment } from './index.js'

const environments = [
  { name: 'Production', slug: 'production' },
  { name: 'Staging', slug: 'staging' },
]

describe('selectTargetEnvironment', () => {
  test('defaults to the first environment when none was requested', () => {
    expect(selectTargetEnvironment(environments)).toBe(environments[0])
  })

  test('matches by name case-insensitively', () => {
    expect(selectTargetEnvironment(environments, 'staging')).toBe(environments[1])
    expect(selectTargetEnvironment(environments, 'STAGING')).toBe(environments[1])
  })

  test('matches by exact slug', () => {
    expect(selectTargetEnvironment(environments, 'production')).toBe(environments[0])
  })

  test('returns undefined rather than silently falling back on no match', () => {
    // A typo'd --environment should surface as "no containers", not quietly
    // show the wrong environment's containers.
    expect(selectTargetEnvironment(environments, 'does-not-exist')).toBeUndefined()
  })

  test('handles an empty environment list', () => {
    expect(selectTargetEnvironment([])).toBeUndefined()
    expect(selectTargetEnvironment([], 'anything')).toBeUndefined()
  })
})
