// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  isComposePresetName,
  shouldLoadComposeSecurity,
} from './compose-security-query'

test('only Docker Compose presets are treated as Compose', () => {
  expect(isComposePresetName('docker-compose')).toBe(true)
  expect(isComposePresetName('dockercompose')).toBe(true)
  for (const preset of [
    'static',
    'dockerfile',
    'nixpacks',
    'nextjs',
    '',
    undefined,
    null,
  ]) {
    expect(isComposePresetName(preset)).toBe(false)
  }
})

test('the compose-security read is gated on a loaded Compose project', () => {
  expect(shouldLoadComposeSecurity({ id: 7, preset: 'docker-compose' })).toBe(
    true
  )
  // Non-Compose projects must never request it (the server answers 400).
  expect(shouldLoadComposeSecurity({ id: 7, preset: 'static' })).toBe(false)
  expect(shouldLoadComposeSecurity({ id: 7, preset: 'dockerfile' })).toBe(false)
  expect(shouldLoadComposeSecurity({ id: 7, preset: null })).toBe(false)
})

test('the compose-security read never fires while the project is loading', () => {
  expect(shouldLoadComposeSecurity(undefined)).toBe(false)
  expect(shouldLoadComposeSecurity(null)).toBe(false)
  expect(shouldLoadComposeSecurity({ preset: 'docker-compose' })).toBe(false)
})
