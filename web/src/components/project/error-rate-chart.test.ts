// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { errorRatePoints, errorRateTone } from './error-rate-chart'

describe('error rate chart', () => {
  test('is drawn in the alarm colour only when something failed', () => {
    expect(errorRateTone(0)).toBe('neutral')
    expect(errorRateTone(undefined)).toBe('neutral')
    expect(errorRateTone(0.002)).toBe('poor')
  })

  test('plots nothing when there were no requests to fail', () => {
    const points = [{ errorRate: 0 }, { errorRate: 0 }]
    expect(errorRatePoints(points, 0)).toEqual([])
    expect(errorRatePoints(points, undefined)).toEqual([])
    expect(errorRatePoints(points, 12)).toBe(points)
  })
})
