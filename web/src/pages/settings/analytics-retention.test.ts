// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ANALYTICS_RETENTION_DEFAULTS,
  ANALYTICS_RETENTION_FIELDS,
  emptyToNull,
  validateRetentionWindow,
} from './analytics-retention'

describe('emptyToNull', () => {
  test('turns a cleared input into "keep" rather than NaN', () => {
    expect(emptyToNull('')).toBeNull()
    expect(emptyToNull(null)).toBeNull()
    expect(emptyToNull(undefined)).toBeNull()
    expect(emptyToNull('abc')).toBeNull()
  })

  test('reads a typed number', () => {
    expect(emptyToNull('30')).toBe(30)
    expect(emptyToNull(90)).toBe(90)
  })
})

describe('validateRetentionWindow', () => {
  test('lets an optional window be left empty to keep the data', () => {
    expect(validateRetentionWindow(null, true)).toBe(true)
  })

  test('requires a window where keeping forever is not allowed', () => {
    expect(validateRetentionWindow(null, false)).toBe(
      'A retention window is required'
    )
  })

  test('accepts whole days from 1 to 3650 only', () => {
    expect(validateRetentionWindow(1, false)).toBe(true)
    expect(validateRetentionWindow(3650, true)).toBe(true)
    for (const bad of [0, -5, 3651, 1.5]) {
      expect(validateRetentionWindow(bad, true)).toBe(
        'Must be a whole number of days from 1 to 3650'
      )
    }
  })
})

describe('analytics retention fields', () => {
  test('events default to one year while optional cleanup stays disabled', () => {
    expect(ANALYTICS_RETENTION_DEFAULTS.events_days).toBe(365)
    expect(ANALYTICS_RETENTION_DEFAULTS.error_events_days).toBeNull()
    expect(ANALYTICS_RETENTION_DEFAULTS.visitor_days).toBeNull()
  })

  test('every setting has a field and a default', () => {
    const names = ANALYTICS_RETENTION_FIELDS.map((field) => field.name).sort()
    expect(names as string[]).toEqual(
      Object.keys(ANALYTICS_RETENTION_DEFAULTS).sort()
    )
  })

  test('a window with no "keep" option always has a numeric default', () => {
    for (const field of ANALYTICS_RETENTION_FIELDS) {
      if (!field.optional) {
        expect(ANALYTICS_RETENTION_DEFAULTS[field.name]).not.toBeNull()
      }
    }
  })

  test('the defaults themselves pass validation', () => {
    for (const field of ANALYTICS_RETENTION_FIELDS) {
      expect(
        validateRetentionWindow(
          ANALYTICS_RETENTION_DEFAULTS[field.name],
          field.optional
        )
      ).toBe(true)
    }
  })

  test('the IP window description does not promise a post-reference clock', () => {
    const ip = ANALYTICS_RETENTION_FIELDS.find(
      (field) => field.name === 'unreferenced_ip_days'
    )
    expect(ip?.description).toContain('not been looked up')
  })
})
