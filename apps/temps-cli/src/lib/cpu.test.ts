// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatMicrocores, parseCores, parseMillicores } from './cpu.js'

describe('parseMillicores', () => {
  test('applies the 0.01-core floor to limits only', () => {
    // Requests are recorded but never applied to Docker, so a tiny one is harmless.
    expect('error' in parseMillicores('5', 'CPU', true)).toBe(true)
    expect(parseMillicores('5', 'CPU request', false)).toEqual({ microcores: 5000 })
  })

  test('tolerates surrounding whitespace', () => {
    expect(parseMillicores(' 250 ', 'CPU', true)).toEqual({ microcores: 250_000 })
  })
})

describe('parseCores', () => {
  test('accepts a leading-dot fraction', () => {
    expect(parseCores('.5', 'CPU limit')).toEqual({ microcores: 500_000 })
  })
})

describe('formatMicrocores', () => {
  test('renders millicores with the equivalent core count', () => {
    expect(formatMicrocores(10_000)).toBe('10m (0.01 CPU)')
    expect(formatMicrocores(1_500_000)).toBe('1500m (1.5 CPU)')
  })
})
