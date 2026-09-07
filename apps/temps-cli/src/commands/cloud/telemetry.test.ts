// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatBytes, formatAge } from './telemetry.js'

// ---------------------------------------------------------------------------
// formatBytes
// ---------------------------------------------------------------------------
//
// NOTE: `describeMode` and the validation branches inside `writeModeSet` are
// not exported from telemetry.ts, so they cannot be covered here without
// modifying production code.  Both functions should be exported (matching the
// pattern in bulk-activation.ts where all helpers are `export function`) so
// that future tests can cover them.  This is flagged for the implementer.

describe('formatBytes', () => {
  test('returns plain bytes below 1 KiB', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(1)).toBe('1 B')
    expect(formatBytes(1023)).toBe('1023 B')
  })

  test('crosses into KiB at exactly 1024 bytes', () => {
    expect(formatBytes(1024)).toBe('1.0 KiB')
  })

  test('renders KiB values up to the MiB threshold', () => {
    // 2 KiB
    expect(formatBytes(2048)).toBe('2.0 KiB')
    // 512 KiB
    expect(formatBytes(524288)).toBe('512.0 KiB')
  })

  test('crosses into MiB at exactly 1 MiB (1024 * 1024)', () => {
    expect(formatBytes(1048576)).toBe('1.0 MiB')
  })

  test('renders MiB values up to the GiB threshold', () => {
    // 10 MiB
    expect(formatBytes(10485760)).toBe('10.0 MiB')
  })

  test('crosses into GiB at exactly 1 GiB (1024 ^ 3)', () => {
    expect(formatBytes(1073741824)).toBe('1.0 GiB')
  })

  test('renders GiB for large values below TiB', () => {
    // 2 GiB
    expect(formatBytes(2147483648)).toBe('2.0 GiB')
  })
})

// ---------------------------------------------------------------------------
// formatAge
// ---------------------------------------------------------------------------

describe('formatAge', () => {
  test('an absent age is not zero', () => {
    // `undefined` means the queue is empty; "0s" would imply everything ships
    // immediately, which is misleading.
    expect(formatAge(undefined)).toBe('—')
  })

  test('renders whole seconds below the minute threshold', () => {
    expect(formatAge(0)).toBe('0s')
    expect(formatAge(1)).toBe('1s')
    expect(formatAge(59)).toBe('59s')
  })

  test('crosses into minutes at exactly 60 seconds', () => {
    expect(formatAge(60)).toBe('1m')
  })

  test('renders minutes up to the hour threshold', () => {
    expect(formatAge(61)).toBe('1m')
    expect(formatAge(3599)).toBe('60m')
  })

  test('crosses into hours at exactly 3600 seconds', () => {
    expect(formatAge(3600)).toBe('1.0h')
  })

  test('renders hours up to the day threshold', () => {
    expect(formatAge(7200)).toBe('2.0h')
    // 86399 s = 23.9997… h → rounds to 24.0h
    expect(formatAge(86399)).toBe('24.0h')
  })

  test('crosses into days at exactly 86400 seconds', () => {
    expect(formatAge(86400)).toBe('1.0d')
  })

  test('renders days for very old spans', () => {
    // 7 days
    expect(formatAge(604800)).toBe('7.0d')
  })
})
