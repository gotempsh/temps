// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  historyTimeRange,
  isValidCheckInterval,
  isValidMonitorType,
  normalizeCheckPath,
  uptimeColor,
} from './index.js'
import { colors } from '../../ui/output.js'

describe('isValidMonitorType', () => {
  test('accepts the known monitor types', () => {
    expect(isValidMonitorType('http')).toBe(true)
    expect(isValidMonitorType('tcp')).toBe(true)
    expect(isValidMonitorType('ping')).toBe(true)
  })

  test('rejects an unsupported type rather than sending it to the API', () => {
    expect(isValidMonitorType('https')).toBe(false)
  })
})

describe('isValidCheckInterval', () => {
  test('accepts the fixed set of interval seconds', () => {
    expect(isValidCheckInterval(60)).toBe(true)
    expect(isValidCheckInterval(1800)).toBe(true)
  })

  test('rejects an arbitrary interval not offered by the backend', () => {
    expect(isValidCheckInterval(120)).toBe(false)
  })
})

describe('normalizeCheckPath', () => {
  test('trims whitespace around a valid path', () => {
    expect(normalizeCheckPath('  /healthz  ')).toBe('/healthz')
  })

  test('passes undefined through untouched', () => {
    expect(normalizeCheckPath(undefined)).toBeUndefined()
  })

  test('rejects a path missing the leading slash', () => {
    // An agent forgetting the leading "/" would otherwise reach the API as
    // an invalid check path with no clear error until the request fails.
    expect(() => normalizeCheckPath('api/healthz')).toThrow(
      /--check-path must start with "\/"/,
    )
  })
})

describe('uptimeColor', () => {
  test('uses success at and above 99%', () => {
    expect(uptimeColor(99)).toBe(colors.success)
    expect(uptimeColor(100)).toBe(colors.success)
  })

  test('uses warning between 95% and 99%', () => {
    expect(uptimeColor(95)).toBe(colors.warning)
    expect(uptimeColor(98.9)).toBe(colors.warning)
  })

  test('uses error below 95%', () => {
    expect(uptimeColor(94.9)).toBe(colors.error)
    expect(uptimeColor(0)).toBe(colors.error)
  })
})

describe('historyTimeRange', () => {
  test('subtracts the requested number of days from a fixed reference date', () => {
    const now = new Date('2026-08-08T12:00:00.000Z')
    const { startTime, endTime } = historyTimeRange(7, now)
    expect(endTime).toBe('2026-08-08T12:00:00.000Z')
    expect(startTime).toBe('2026-08-01T12:00:00.000Z')
  })

  test('handles a range spanning a month boundary', () => {
    const now = new Date('2026-03-02T00:00:00.000Z')
    const { startTime } = historyTimeRange(5, now)
    expect(startTime).toBe('2026-02-25T00:00:00.000Z')
  })
})
