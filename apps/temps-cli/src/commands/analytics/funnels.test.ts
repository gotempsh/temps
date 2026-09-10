// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatDuration, renderConversionBar } from './funnels.js'

describe('formatDuration', () => {
  test('renders sub-minute durations in seconds', () => {
    expect(formatDuration(0)).toBe('0.0s')
    expect(formatDuration(45)).toBe('45.0s')
  })

  test('renders 60s-59m59s durations in minutes', () => {
    expect(formatDuration(60)).toBe('1.0m')
    expect(formatDuration(125)).toBe('2.1m')
  })

  test('a value just under the hour boundary can round up to "60.0m"', () => {
    // 3599s is still < 3600 so it takes the minutes branch, and
    // (3599/60).toFixed(1) rounds to "60.0" — a real display quirk right at
    // the boundary, not a bug this test is meant to hide.
    expect(formatDuration(3599)).toBe('60.0m')
  })

  test('renders durations at and above one hour in hours', () => {
    expect(formatDuration(3600)).toBe('1.0h')
    expect(formatDuration(7325)).toBe('2.0h')
  })
})

describe('renderConversionBar', () => {
  test('0% conversion renders an all-empty bar', () => {
    expect(renderConversionBar(0)).toBe('░'.repeat(20))
  })

  test('100% conversion renders an all-filled bar', () => {
    expect(renderConversionBar(100)).toBe('█'.repeat(20))
  })

  test('splits the bar proportionally to the rate', () => {
    expect(renderConversionBar(50)).toBe('█'.repeat(10) + '░'.repeat(10))
  })

  test('honors a custom width', () => {
    expect(renderConversionBar(25, 10)).toBe('█'.repeat(3) + '░'.repeat(7))
  })
})
