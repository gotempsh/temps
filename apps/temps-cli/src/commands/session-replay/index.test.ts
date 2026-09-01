// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatDuration, deviceIcon, formatSessionUrl, eventTypeName } from './index.js'

describe('formatDuration', () => {
  test('renders missing durations as an em dash, not 0s', () => {
    expect(formatDuration(null)).toBe('—')
    expect(formatDuration(undefined)).toBe('—')
  })

  test('renders sub-minute durations as seconds', () => {
    expect(formatDuration(0)).toBe('0s')
    expect(formatDuration(5000)).toBe('5s')
    expect(formatDuration(59000)).toBe('59s')
  })

  test('rounds to the nearest second', () => {
    expect(formatDuration(5999)).toBe('6s')
  })

  test('renders whole minutes without a trailing 0s', () => {
    expect(formatDuration(60_000)).toBe('1m')
  })

  test('renders minutes with a remainder as "Xm Ys"', () => {
    expect(formatDuration(90_000)).toBe('1m 30s')
    expect(formatDuration(125_000)).toBe('2m 5s')
  })
})

describe('deviceIcon', () => {
  test('renders a placeholder when device is unknown', () => {
    expect(deviceIcon(null)).toBe('?')
    expect(deviceIcon(undefined)).toBe('?')
    expect(deviceIcon('')).toBe('?')
  })

  test('matches mobile and tablet case-insensitively', () => {
    expect(deviceIcon('Mobile')).toBe('📱')
    expect(deviceIcon('mobile')).toBe('📱')
    expect(deviceIcon('Tablet')).toBe('📟')
  })

  test('anything else, including an unrecognized value, falls back to desktop', () => {
    expect(deviceIcon('Desktop')).toBe('🖥')
    expect(deviceIcon('Smart TV')).toBe('🖥')
  })
})

describe('formatSessionUrl', () => {
  test('renders a missing URL as an em dash', () => {
    expect(formatSessionUrl(null)).toBe('—')
    expect(formatSessionUrl(undefined)).toBe('—')
    expect(formatSessionUrl('')).toBe('—')
  })

  test('strips scheme and host, keeping only the path', () => {
    expect(formatSessionUrl('https://example.com/foo/bar')).toBe('/foo/bar')
    expect(formatSessionUrl('http://example.com:3000/path')).toBe('/path')
  })

  test('falls back to "/" for a bare origin with no path', () => {
    // Stripping scheme+host from a bare origin leaves an empty string, which
    // would otherwise render as a blank, confusing table cell.
    expect(formatSessionUrl('https://example.com')).toBe('/')
  })

  test('leaves an explicit root path as "/"', () => {
    expect(formatSessionUrl('https://example.com/')).toBe('/')
  })
})

describe('eventTypeName', () => {
  test('maps known rrweb event type codes to their names', () => {
    expect(eventTypeName(0)).toBe('DomContentLoaded')
    expect(eventTypeName(1)).toBe('Load')
    expect(eventTypeName(2)).toBe('FullSnapshot')
    expect(eventTypeName(3)).toBe('IncrementalSnapshot')
    expect(eventTypeName(4)).toBe('Meta')
    expect(eventTypeName(5)).toBe('Custom')
    expect(eventTypeName(6)).toBe('Plugin')
  })

  test('renders a missing type as an em dash', () => {
    expect(eventTypeName(null)).toBe('—')
    expect(eventTypeName(undefined)).toBe('—')
  })

  test('renders an unknown type code rather than dropping it silently', () => {
    expect(eventTypeName(99)).toBe('Type99')
  })
})
