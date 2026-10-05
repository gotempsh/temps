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
  diskProjectionCaption,
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
  const TB = 1e12
  const hourly = (values: number[]) =>
    values.map((value, h) => ({ time: t(h * 3600), value }))

  test('fits a line and reports days to the 90% line and to full', () => {
    // 1 GB/hour on a 100 GB disk, from 50 GB, over 7 hours.
    const p = projectDisk(
      hourly([0, 1, 2, 3, 4, 5, 6, 7].map((h) => 50 * GB + h * GB)),
      100 * GB
    )
    expect(p?.kind).toBe('growing')
    if (p?.kind !== 'growing') throw new Error('expected growth')
    expect(p.bytesPerDay).toBeCloseTo(24 * GB, -6)
    expect(p.spanMs).toBe(7 * 3_600_000)
    // last sample 57 GB: 33 GB to the 90 GB line, 43 GB to full
    expect(p.daysToLine).toBeCloseTo(33 / 24, 2)
    expect(p.daysToFull).toBeCloseTo(43 / 24, 2)
    expect(diskProjectionCaption(43 * GB, p)).toBe(
      '43.0 GB free · growing 24.0 GB/day over the last 7.0 h, full in 2 days'
    )
  })

  test('a few minutes of samples never produce a projection', () => {
    // The reported case: a 2.5 GB image pull inside a few minutes of a dev
    // machine's history used to read "growing 1.21 TB/day, full in 3 days".
    const points = [0, 1, 2, 3, 4, 5, 6].map((m) => ({
      time: t(m * 60),
      value: 500 * GB + (m >= 3 ? 2.5 * GB : 0),
    }))
    const p = projectDisk(points, 4 * TB)
    expect(p).toEqual({ kind: 'insufficient', samples: 7, spanMs: 6 * 60_000 })
    expect(diskProjectionCaption(3 * TB, p)).toBe(
      '3.00 TB free · collecting history to project growth (needs 6 h)'
    )
  })

  test('needs six samples, six hours and a total', () => {
    expect(projectDisk(hourly([1, 2, 3]), 100)?.kind).toBe('insufficient')
    expect(
      projectDisk(
        [0, 1, 2, 3, 4, 5].map((m) => ({ time: t(m * 600), value: m })),
        100
      )?.kind
    ).toBe('insufficient')
    expect(projectDisk(hourly([1, 2, 3, 4, 5, 6, 7]), 0)).toBeNull()
    expect(projectDisk(undefined, 100)?.kind).toBe('insufficient')
  })

  test('a flat or shrinking disk is steady', () => {
    const flat = projectDisk(
      hourly([10, 10, 10, 10, 10, 10, 10].map((v) => v * GB)),
      100 * GB
    )
    expect(flat).toEqual({ kind: 'steady', spanMs: 6 * 3_600_000 })
    const shrinking = projectDisk(
      hourly([10, 9, 8, 7, 6, 5, 4].map((v) => v * GB)),
      100 * GB
    )
    expect(shrinking?.kind).toBe('steady')
    expect(diskProjectionCaption(90 * GB, shrinking)).toBe(
      '90.0 GB free · no steady growth over the last 6.0 h'
    )
  })

  test('usage that goes up and down without a trend is steady', () => {
    const noisy = projectDisk(
      hourly([10, 14, 9, 13, 10, 14, 9, 13, 11].map((v) => v * GB)),
      100 * GB
    )
    expect(noisy?.kind).toBe('steady')
  })

  test('a rate that would fill the whole disk within a day is not projected', () => {
    // 2 TB/day sustained on a 1 TB volume.
    const p = projectDisk(
      hourly([0, 1, 2, 3, 4, 5, 6].map((h) => 100 * GB + (h * (2 * TB)) / 24)),
      1 * TB
    )
    expect(p?.kind).toBe('unreliable')
    expect(diskProjectionCaption(500 * GB, p)).toBe(
      '500 GB free · usage jumped recently; too irregular to project'
    )
  })

  test('formatDays reads in planning units', () => {
    expect(formatDays(Infinity)).toBe('not growing')
    expect(formatDays(0.5)).toBe('less than a day')
    expect(formatDays(44)).toBe('44 days')
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
