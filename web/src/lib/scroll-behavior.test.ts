// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { preferredScrollBehavior } from './scroll-behavior'

const original = globalThis.window

afterEach(() => {
  globalThis.window = original
})

function withReducedMotion(reduce: boolean) {
  globalThis.window = {
    matchMedia: (query: string) => ({
      matches: reduce && query === '(prefers-reduced-motion: reduce)',
    }),
  } as unknown as Window & typeof globalThis
}

test('scrolls smoothly by default', () => {
  withReducedMotion(false)
  expect(preferredScrollBehavior()).toBe('smooth')
})

test('jumps instantly when the user prefers reduced motion', () => {
  withReducedMotion(true)
  expect(preferredScrollBehavior()).toBe('auto')
})
