// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { getRelativeTime } from './deploy.js'

describe('getRelativeTime', () => {
  test('renders anything under a minute as "just now"', () => {
    expect(getRelativeTime(new Date(Date.now() - 5_000))).toBe('just now')
  })

  test('renders minutes ago under an hour', () => {
    expect(getRelativeTime(new Date(Date.now() - 5 * 60_000))).toBe('5m ago')
  })

  test('renders hours ago under a day', () => {
    expect(getRelativeTime(new Date(Date.now() - 3 * 60 * 60_000))).toBe('3h ago')
  })

  test('renders days ago beyond a day', () => {
    expect(getRelativeTime(new Date(Date.now() - 2 * 24 * 60 * 60_000))).toBe('2d ago')
  })
})
