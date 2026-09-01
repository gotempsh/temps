// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatPrice, usageBar } from './billing.js'

describe('formatPrice', () => {
  test('renders zero cents as free rather than €0.00/mo', () => {
    expect(formatPrice(0)).toContain('free')
  })

  test('converts cents to euros with two decimals', () => {
    expect(formatPrice(600)).toContain('€6.00/mo')
  })

  test('rounds fractional cents to two decimal places', () => {
    expect(formatPrice(1050)).toContain('€10.50/mo')
  })
})

describe('usageBar', () => {
  test('renders a fully-empty bar at zero usage', () => {
    const bar = usageBar(0, 20)
    expect(bar).toContain('0/20')
    expect(bar).toContain('░'.repeat(20))
  })

  test('renders a fully-filled bar at full usage', () => {
    const bar = usageBar(20, 20)
    expect(bar).toContain('20/20')
    expect(bar).toContain('█'.repeat(20))
  })

  test('clamps usage above the limit rather than overflowing the bar', () => {
    const bar = usageBar(50, 20)
    expect(bar).toContain('50/20')
    expect(bar).toContain('█'.repeat(20))
    expect(bar).not.toContain('░')
  })

  test('treats a zero limit as empty rather than dividing by zero', () => {
    const bar = usageBar(0, 0)
    expect(bar).toContain('0/0')
    expect(bar).toContain('░'.repeat(20))
  })

  test('renders a partial bar proportional to usage', () => {
    const bar = usageBar(10, 20)
    expect(bar).toContain('10/20')
    expect(bar).toContain('█'.repeat(10))
    expect(bar).toContain('░'.repeat(10))
  })
})
