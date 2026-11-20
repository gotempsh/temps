// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  alertPresetFromParams,
  crashFreeAlertHref,
  crashFreeChartRows,
  CRASH_FREE_SESSIONS_METRIC,
  formatRate,
  healthDaysFromParam,
  rateTone,
  releaseLabel,
} from './release-health'

describe('formatRate', () => {
  test('never rounds a rate below 100% up to 100%', () => {
    expect(formatRate(0.99999)).toBe('99.99%')
    expect(formatRate(1)).toBe('100%')
    expect(formatRate(0.95)).toBe('95.0%')
    expect(formatRate(0.9512)).toBe('95.1%')
    expect(formatRate(null)).toBe('—')
  })
})

describe('rateTone', () => {
  test('bands crash-free rates', () => {
    expect(rateTone(0.97)).toBe('critical')
    expect(rateTone(0.99)).toBe('warning')
    expect(rateTone(0.999)).toBe('healthy')
    expect(rateTone(undefined)).toBe('unknown')
  })
})

describe('releaseLabel', () => {
  test('shortens commit SHAs only', () => {
    expect(releaseLabel('0123456789abcdef0123456789abcdef01234567')).toBe(
      '0123456789ab'
    )
    expect(releaseLabel('my-app@1.2.3')).toBe('my-app@1.2.3')
  })
})

describe('healthDaysFromParam', () => {
  test('accepts the offered windows and falls back to 7', () => {
    expect(healthDaysFromParam('14')).toBe(14)
    expect(healthDaysFromParam('31')).toBe(7)
    expect(healthDaysFromParam(null)).toBe(7)
  })
})

describe('crash-free alert preset', () => {
  test('the link opens a per-release rule on the published gauge', () => {
    const href = crashFreeAlertHref('synthetic-project')
    const preset = alertPresetFromParams(
      new URL(href, 'https://temps.test').searchParams
    )
    expect(
      href.startsWith('/projects/synthetic-project/metrics/alerts/new?')
    ).toBe(true)
    expect(preset?.metric_name).toBe(CRASH_FREE_SESSIONS_METRIC)
    expect(preset?.comparator).toBe('lt')
    expect(preset?.threshold).toBe(0.99)
    expect(preset?.group_by).toEqual(['release'])
    expect(preset?.dynamic_alerts).toBe(true)
  })

  test('any other URL leaves the form empty', () => {
    expect(
      alertPresetFromParams(new URLSearchParams('preset=other'))
    ).toBeNull()
    expect(alertPresetFromParams(new URLSearchParams())).toBeNull()
  })
})

describe('crashFreeChartRows', () => {
  test('turns rates into percentages and keeps hours without a rate', () => {
    expect(
      crashFreeChartRows([
        {
          bucket_start: '2026-11-20T10:00:00Z',
          sessions: 100,
          crash_free_sessions: 0.95,
        },
        {
          bucket_start: '2026-11-20T11:00:00Z',
          sessions: 0,
          crash_free_sessions: null,
        },
      ])
    ).toMatchObject([
      { time: '2026-11-20T10:00:00Z', sessions: 100, crashFree: 95 },
      { time: '2026-11-20T11:00:00Z', sessions: 0, crashFree: null },
    ])
  })
})
