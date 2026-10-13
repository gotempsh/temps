// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  buildYAxisTicks,
  formatAxisNumber,
  stepDecimals,
  yAxisTickProps,
  type AxisTickFormatter,
} from './chart-axis-ticks'

const labels = (ticks: number[], format: (v: number) => string) =>
  ticks.map(format)

const distinct = (values: string[]) => new Set(values).size === values.length

describe('stepDecimals', () => {
  test('counts the places a step needs', () => {
    expect(stepDecimals(1)).toBe(0)
    expect(stepDecimals(50)).toBe(0)
    expect(stepDecimals(0.5)).toBe(1)
    expect(stepDecimals(0.25)).toBe(2)
    expect(stepDecimals(0.002)).toBe(3)
    expect(stepDecimals(0)).toBe(0)
    expect(stepDecimals(Number.NaN)).toBe(0)
  })
})

describe('formatAxisNumber', () => {
  test('prints fixed places and never a negative zero', () => {
    expect(formatAxisNumber(0.25, 2)).toBe('0.25')
    expect(formatAxisNumber(1, 1)).toBe('1.0')
    expect(formatAxisNumber(-0.0001, 1)).toBe('0.0')
    expect(formatAxisNumber(Number.NaN, 1)).toBe('')
  })
})

describe('buildYAxisTicks', () => {
  test('a small percentage range gets distinct, precise ticks', () => {
    // An error rate peaking at 1.3%: recharts' own ticks printed as
    // "0%, 0%, 1%, 1%, 1%".
    const axis = buildYAxisTicks(0, 1.3)
    expect(axis.ticks).toEqual([0, 0.5, 1, 1.5])
    expect(axis.decimals).toBe(1)
    const printed = labels(
      axis.ticks,
      (v) => `${formatAxisNumber(v, axis.decimals)}%`
    )
    expect(printed).toEqual(['0.0%', '0.5%', '1.0%', '1.5%'])
    expect(axis.domain).toEqual([0, 1.5])
  })

  test('widens the step when the caller rounds to whole numbers', () => {
    // A connected-clients gauge sitting at 1 with a Math.round formatter.
    const round: AxisTickFormatter = (v) => String(Math.round(v))
    const axis = buildYAxisTicks(0, 1, { format: round })
    const printed = labels(axis.ticks, (v) => round(v, axis))
    expect(distinct(printed)).toBe(true)
    expect(axis.ticks).toEqual([0, 1])
  })

  test('integer axes never produce fractional ticks', () => {
    const axis = buildYAxisTicks(0, 3, { integer: true })
    expect(axis.ticks.every(Number.isInteger)).toBe(true)
    expect(axis.ticks).toEqual([0, 1, 2, 3])
  })

  test('respects the tick budget on large ranges', () => {
    const axis = buildYAxisTicks(0, 987_654, { maxTicks: 5 })
    expect(axis.ticks.length).toBeLessThanOrEqual(5)
    expect(axis.ticks[0]).toBe(0)
    expect(axis.ticks[axis.ticks.length - 1]).toBeGreaterThanOrEqual(987_654)
    expect(axis.decimals).toBe(0)
  })

  test('tiny ranges keep enough decimals to stay distinct', () => {
    const axis = buildYAxisTicks(0, 0.003)
    const printed = labels(axis.ticks, (v) =>
      formatAxisNumber(v, axis.decimals)
    )
    expect(distinct(printed)).toBe(true)
    expect(axis.decimals).toBeGreaterThanOrEqual(3)
  })

  test('a flat or empty series still yields a usable axis', () => {
    expect(buildYAxisTicks(0, 0).ticks).toEqual([0, 0.2, 0.4, 0.6, 0.8, 1])
    const flat = buildYAxisTicks(42, 42, { includeZero: false })
    expect(flat.ticks[0]).toBeLessThan(42)
    expect(flat.ticks[flat.ticks.length - 1]).toBeGreaterThan(42)
    const nonFinite = buildYAxisTicks(Number.NaN, Number.POSITIVE_INFINITY)
    expect(nonFinite.ticks.length).toBeGreaterThanOrEqual(2)
  })

  test('covers negative values', () => {
    const axis = buildYAxisTicks(-3, 7)
    expect(axis.ticks[0]).toBeLessThanOrEqual(-3)
    expect(axis.ticks[axis.ticks.length - 1]).toBeGreaterThanOrEqual(7)
    expect(axis.ticks).toContain(0)
  })

  test('ticks carry no float noise', () => {
    const axis = buildYAxisTicks(0, 0.7)
    for (const tick of axis.ticks) {
      expect(String(tick).length).toBeLessThanOrEqual(5)
    }
  })
})

describe('yAxisTickProps', () => {
  test('derives the range from data and formats with the step precision', () => {
    const props = yAxisTickProps([0.2, 1.3, null, undefined, Number.NaN], {
      format: (v, ctx) => `${formatAxisNumber(v, ctx.decimals)}%`,
    })
    expect(props.ticks).toEqual([0, 0.5, 1, 1.5])
    expect(props.domain).toEqual([0, 1.5])
    expect(props.ticks.map(props.tickFormatter)).toEqual([
      '0.0%',
      '0.5%',
      '1.0%',
      '1.5%',
    ])
  })

  test('an all-ones client count prints distinct whole numbers', () => {
    const props = yAxisTickProps([1, 1, 1], {
      format: (v) => Math.round(v).toString(),
    })
    const printed = props.ticks.map(props.tickFormatter)
    expect(distinct(printed)).toBe(true)
    expect(printed).toEqual(['0', '1'])
  })
})
