// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { formatLifetime } from './lifetime'

test('lifetimes use the largest unit that stays exact', () => {
  expect(formatLifetime(30)).toBe('30 s')
  expect(formatLifetime(60)).toBe('1 min')
  expect(formatLifetime(900)).toBe('15 min')
  expect(formatLifetime(90)).toBe('90 s')
  expect(formatLifetime(3600)).toBe('1 h')
  expect(formatLifetime(5400)).toBe('1 h 30 min')
  expect(formatLifetime(3661)).toBe('3661 s')
})

test('an empty or invalid lifetime never renders NaN', () => {
  expect(formatLifetime(0)).toBe('0 s')
  expect(formatLifetime(-5)).toBe('0 s')
  expect(formatLifetime(Number.NaN)).toBe('0 s')
})
