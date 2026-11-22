// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  environmentShieldModeToPayload,
  environmentShieldModeToSelect,
  exemptPathsToText,
  formatSolveMs,
  isoToLocalInput,
  localInputToIso,
  parseExemptPathsText,
  SHIELD_MODES,
  shieldEventKindLabel,
  shieldModeEnforces,
  shieldVerdictBadge,
  validateExemptPaths,
} from './shield'

describe('shield modes', () => {
  test('lists the four modes in order, one line each', () => {
    expect(SHIELD_MODES.map((mode) => mode.value)).toEqual([
      'off',
      'log',
      'selective',
      'attack',
    ])
    for (const mode of SHIELD_MODES) {
      expect(mode.description).not.toContain('\n')
    }
  })

  test('only selective and attack enforce', () => {
    expect(shieldModeEnforces('off')).toBe(false)
    expect(shieldModeEnforces('log')).toBe(false)
    expect(shieldModeEnforces('selective')).toBe(true)
    expect(shieldModeEnforces('attack')).toBe(true)
  })

  test('an environment without a mode inherits, and inherit clears it', () => {
    expect(environmentShieldModeToSelect(null)).toBe('inherit')
    expect(environmentShieldModeToSelect(undefined)).toBe('inherit')
    expect(environmentShieldModeToSelect('selective')).toBe('selective')
    expect(environmentShieldModeToPayload('inherit')).toBeNull()
    expect(environmentShieldModeToPayload('off')).toBe('off')
  })
})

describe('exempt paths', () => {
  test('round trips through the text area, dropping blank lines', () => {
    const paths = parseExemptPathsText('/api/*\n\n  /health  \n')
    expect(paths).toEqual(['/api/*', '/health'])
    expect(exemptPathsToText(paths)).toBe('/api/*\n/health')
    expect(exemptPathsToText(null)).toBe('')
  })

  test('accepts exact paths and a trailing /*', () => {
    expect(validateExemptPaths(['/api/*', '/health', '/*'])).toBeNull()
  })

  test('refuses what the server refuses', () => {
    expect(validateExemptPaths(['api/*'])).toContain('must start with "/"')
    expect(validateExemptPaths(['/api*'])).toContain('/*')
    expect(validateExemptPaths(['/a/*/b'])).toContain('/*')
    expect(validateExemptPaths(['/*/*'])).toContain('/*')
    const tooMany = Array.from({ length: 65 }, (_, i) => `/p${i}`)
    expect(validateExemptPaths(tooMany)).toContain('64')
  })
})

test('solve times read in ms below a second and seconds above', () => {
  expect(formatSolveMs(null)).toBe('—')
  expect(formatSolveMs(412.4)).toBe('412 ms')
  expect(formatSolveMs(2300)).toBe('2.3 s')
})

test('verdict badges keep block, challenge and allow apart', () => {
  expect(shieldVerdictBadge('block')).toBe('destructive-subtle')
  expect(shieldVerdictBadge('challenge')).toBe('warning-subtle')
  expect(shieldVerdictBadge('allow')).toBe('success-subtle')
  expect(shieldVerdictBadge('log')).toBe('neutral-subtle')
  expect(shieldEventKindLabel('challenge_solved')).toBe('Challenge solved')
  expect(shieldEventKindLabel('something_new')).toBe('something_new')
})

test('expiry inputs round trip through local time', () => {
  expect(isoToLocalInput(null)).toBe('')
  expect(localInputToIso('')).toBeNull()
  const local = isoToLocalInput('2026-10-27T10:30:00.000Z')
  expect(localInputToIso(local)).toBe('2026-10-27T10:30:00.000Z')
})
