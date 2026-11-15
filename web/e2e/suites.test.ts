// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { join } from 'node:path'

import {
  missingSmokeSpecs,
  selectedSuite,
  SMOKE_SPECS,
  specMatcher,
} from './suites'

describe('e2e suites', () => {
  test('every smoke spec exists', () => {
    expect(missingSmokeSpecs(join(import.meta.dir))).toEqual([])
  })

  test('smoke specs are not listed twice', () => {
    const all = [...SMOKE_SPECS.anonymous, ...SMOKE_SPECS.authenticated]
    expect(new Set(all).size).toBe(all.length)
  })

  test('suite selection defaults to full and rejects typos', () => {
    expect(selectedSuite(undefined)).toBe('full')
    expect(selectedSuite('')).toBe('full')
    expect(selectedSuite('full')).toBe('full')
    expect(selectedSuite('smoke')).toBe('smoke')
    expect(() => selectedSuite('smoke ')).toThrow('E2E_SUITE')
    expect(() => selectedSuite('Smoke')).toThrow('E2E_SUITE')
  })

  test('matcher selects exact file names only', () => {
    const matcher = specMatcher(['auth.spec.ts'])
    expect(matcher.test('/repo/web/e2e/anonymous/auth.spec.ts')).toBe(true)
    expect(matcher.test('C:\\repo\\web\\e2e\\anonymous\\auth.spec.ts')).toBe(
      true
    )
    expect(matcher.test('/repo/web/e2e/anonymous/oauth.spec.ts')).toBe(false)
    expect(matcher.test('/repo/web/e2e/anonymous/auth.spec.ts.bak')).toBe(false)
  })
})
