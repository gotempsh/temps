// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  CPU_THRESHOLDS,
  formatBytesBinary,
  formatBytesDecimal,
  formatBytesPerSecond,
  formatDays,
  formatRateTick,
  isMetricsUnavailable,
  mergeSeries,
  peakOf,
  projectDisk,
  toRatePerSecond,
  usagePercent,
  usageTone,
} from './server-monitoring'

const T0 = 1_700_000_000_000
const t = (secs: number) => new Date(T0 + secs * 1000).toISOString()

describe('mergeSeries', () => {
  test('joins series on timestamp, labels rows and sorts ascending', () => {
    const rows = mergeSeries(
      [
        {
          key: 'rx',
          points: [
            { time: t(60), value: 2 },
            { time: t(0), value: 1 },
          ],
        },
        { key: 'tx', points: [{ time: t(60), value: 5 }] },
      ],
      (iso) => `L${iso.slice(14, 16)}`
    )
    expect(rows).toEqual([
      { time: t(0), label: 'L13', rx: 1 },
      { time: t(60), label: 'L14', rx: 2, tx: 5 },
    ])
  })

  test('skips unparsable timestamps and undefined series', () => {
    const rows = mergeSeries(
      [
        { key: 'a', points: [{ time: 'garbage', value: 1 }] },
        { key: 'b', points: undefined },
      ],
      () => ''
    )
    expect(rows).toEqual([])
  })
})

describe('toRatePerSecond', () => {
  test('divides per-bucket increase by the bucket width', () => {
    const rate = toRatePerSecond(
      [
        { time: t(0), value: 6000 },
        { time: t(60), value: 12000 },
        { time: t(120), value: -5 },
      ],
      30
    )
    expect(rate.map((p) => p.value)).toEqual([100, 200, 0])
  })

  test('a single point uses the fallback step', () => {
    expect(toRatePerSecond([{ time: t(0), value: 300 }], 30)[0].value).toBe(10)
    expect(toRatePerSecond(undefined, 30)).toEqual([])
  })
})

describe('projectDisk', () => {
  const GB = 1e9
  test('fits a line and reports days to the 90% line and to full', () => {
    // 1 GB/hour on a 100 GB disk, from 50 GB.
    const points = [0, 1, 2, 3].map((h) => ({
      time: t(h * 3600),
      value: 50 * GB + h * GB,
    }))
    const p = projectDisk(points, 100 * GB)
    expect(p).not.toBeNull()
    expect(p!.bytesPerDay).toBeCloseTo(24 * GB, -6)
    // last sample 53 GB: 37 GB to the 90 GB line, 47 GB to full
    expect(p!.daysToLine).toBeCloseTo(37 / 24, 2)
    expect(p!.daysToFull).toBeCloseTo(47 / 24, 2)
  })

  test('a flat or shrinking disk is not growing', () => {
    const p = projectDisk(
      [
        { time: t(0), value: 10 * GB },
        { time: t(60), value: 9 * GB },
        { time: t(120), value: 8 * GB },
      ],
      100 * GB
    )
    expect(p!.bytesPerDay).toBeLessThan(0)
    expect(p!.daysToFull).toBe(Infinity)
    expect(formatDays(p!.daysToFull)).toBe('not growing')
  })

  test('needs three samples and a total', () => {
    expect(projectDisk([{ time: t(0), value: 1 }], 100)).toBeNull()
    expect(
      projectDisk(
        [
          { time: t(0), value: 1 },
          { time: t(1), value: 2 },
          { time: t(2), value: 3 },
        ],
        0
      )
    ).toBeNull()
  })
})

describe('thresholds and peaks', () => {
  test('usageTone picks the highest line at or under the value', () => {
    expect(usageTone(50, CPU_THRESHOLDS)).toBe('good')
    expect(usageTone(80, CPU_THRESHOLDS)).toBe('warn')
    expect(usageTone(95, CPU_THRESHOLDS)).toBe('poor')
    expect(usageTone(null, CPU_THRESHOLDS)).toBe('good')
  })

  test('peakOf finds the highest sample', () => {
    expect(
      peakOf([
        { time: t(0), value: 3 },
        { time: t(60), value: 9 },
        { time: t(120), value: 4 },
      ])
    ).toEqual({ time: t(60), value: 9 })
    expect(peakOf([])).toBeNull()
  })
})

describe('formatters', () => {
  test('binary units', () => {
    expect(formatBytesBinary(0)).toBe('0 B')
    expect(formatBytesBinary(1536)).toBe('1.50 KiB')
    expect(formatBytesBinary(2 * 1024 ** 3)).toBe('2.00 GiB')
    expect(formatBytesBinary(null)).toBe('—')
  })

  test('decimal units', () => {
    expect(formatBytesDecimal(13_930_000_000)).toBe('13.9 GB')
    expect(formatBytesDecimal(undefined)).toBe('—')
  })

  test('throughput', () => {
    expect(formatBytesPerSecond(1024)).toBe('1.00 KiB/s')
    expect(formatBytesPerSecond(NaN)).toBe('—')
  })

  test('compact rate ticks', () => {
    expect(formatRateTick(512)).toBe('512')
    expect(formatRateTick(1536)).toBe('1.5k')
    expect(formatRateTick(296 * 1024 ** 2)).toBe('296M')
    expect(formatRateTick(2.5 * 1024 ** 3)).toBe('2.5G')
  })

  test('usagePercent clamps and guards zero totals', () => {
    expect(usagePercent(50, 200)).toBe(25)
    expect(usagePercent(300, 200)).toBe(100)
    expect(usagePercent(5, 0)).toBe(0)
  })

  test('formatDays speaks in days, months, years', () => {
    expect(formatDays(0.5)).toBe('less than a day')
    expect(formatDays(12)).toBe('12 days')
    expect(formatDays(120)).toBe('4 months')
    expect(formatDays(1000)).toBe('3 years')
  })
})

describe('isMetricsUnavailable', () => {
  test('503 or "not enabled" problem details mean not configured', () => {
    expect(isMetricsUnavailable({ status: 503 })).toBe(true)
    expect(
      isMetricsUnavailable({
        status: 500,
        detail: 'Metric collection is not enabled on this server',
      })
    ).toBe(true)
    expect(isMetricsUnavailable({ status: 500, detail: 'boom' })).toBe(false)
    expect(isMetricsUnavailable(undefined)).toBe(false)
  })
})
