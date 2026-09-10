// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  PASSWORD_REQUIREMENTS,
  generateCompliantPassword,
  passwordRequirementResults,
  passwordSchema,
} from './password-policy'

describe('password policy', () => {
  test('matches every server-side password requirement', () => {
    expect(PASSWORD_REQUIREMENTS.map((requirement) => requirement.id)).toEqual([
      'length',
      'uppercase',
      'lowercase',
      'number',
      'special',
    ])
    expect(passwordSchema.safeParse('ValidPass1!').success).toBe(true)
  })

  test('reports unmet requirements independently', () => {
    const results = passwordRequirementResults('password')
    expect(
      results.filter((result) => result.met).map((result) => result.id)
    ).toEqual(['length', 'lowercase'])
  })

  test('generates random passwords that meet the complete policy', () => {
    const generated = Array.from({ length: 20 }, () =>
      generateCompliantPassword()
    )

    expect(generated.every((password) => password.length === 20)).toBe(true)
    expect(
      generated.every((password) => passwordSchema.safeParse(password).success)
    ).toBe(true)
    expect(new Set(generated).size).toBe(generated.length)
  })

  test('rejects generated password lengths outside the server limits', () => {
    expect(() => generateCompliantPassword(7)).toThrow(RangeError)
    expect(() => generateCompliantPassword(129)).toThrow(RangeError)
  })
})
