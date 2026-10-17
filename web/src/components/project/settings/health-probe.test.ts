// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  parseHealthCommand,
  healthProbeFormSchema,
  healthConfigFromForm,
} from './health-probe'

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

describe('switching health probe controls', () => {
  const values = {
    probeType: 'http' as const,
    stableFor: 15,
    probeTimeout: NaN,
    probeInterval: NaN,
    probeCommand: '',
    healthPath: '/',
  }
  test('a cleared TCP timeout does not block HTTP and is normalized before saving', () => {
    expect(healthProbeFormSchema.safeParse(values).success).toBe(true)
    expect(healthConfigFromForm(values).timeout).toBe(5)
    expect(
      healthProbeFormSchema.safeParse({ ...values, probeType: 'tcp' }).success
    ).toBe(false)
  })
  test('a cleared HTTP path does not block a process probe', () => {
    const process = { ...values, probeType: 'process' as const, healthPath: '' }
    expect(healthProbeFormSchema.safeParse(process).success).toBe(true)
    expect(healthConfigFromForm(process).path).toBe('/')
    expect(
      healthProbeFormSchema.safeParse({ ...process, probeType: 'http' }).success
    ).toBe(false)
  })
  test('exec and process still validate their visible fields', () => {
    expect(
      healthProbeFormSchema.safeParse({
        ...values,
        probeType: 'process',
        stableFor: NaN,
      }).success
    ).toBe(false)
    expect(
      healthProbeFormSchema.safeParse({
        ...values,
        probeType: 'exec',
        probeTimeout: 5,
        probeInterval: 1,
      }).success
    ).toBe(false)
  })
})
