// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { preferredScrollBehavior } from './scroll-behavior'

const system = (reduce: boolean) => (query: string) => ({
  matches: reduce && query === '(prefers-reduced-motion: reduce)',
})

test('scrolls smoothly by default', () => {
  expect(preferredScrollBehavior(system(false))).toBe('smooth')
})

test('jumps instantly when the user prefers reduced motion', () => {
  expect(preferredScrollBehavior(system(true))).toBe('auto')
})

test('scrolls smoothly where the system cannot say', () => {
  // `null`, not `undefined`: undefined would fall back to the real system.
  expect(preferredScrollBehavior(null)).toBe('smooth')
})
