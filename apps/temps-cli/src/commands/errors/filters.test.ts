// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildFilterUpdate, checkFilterPattern, parseFilterKind } from './filters.js'

describe('parseFilterKind', () => {
  test('accepts dashes, underscores and any case', () => {
    expect(parseFilterKind('error-message')).toBe('error_message')
    expect(parseFilterKind('WEB_CRAWLERS')).toBe('web_crawlers')
    expect(parseFilterKind(' localhost ')).toBe('localhost')
  })

  test('names the kinds it knows when one is unknown', () => {
    expect(() => parseFilterKind('bots')).toThrow(/browser-extensions, web-crawlers, localhost, error-message, release/)
  })
})

describe('checkFilterPattern', () => {
  test('pattern kinds need a pattern and get it trimmed', () => {
    expect(checkFilterPattern('error_message', '  *ResizeObserver*  ')).toBe('*ResizeObserver*')
    expect(() => checkFilterPattern('release', undefined)).toThrow(/needs --pattern/)
    expect(() => checkFilterPattern('release', '   ')).toThrow(/needs --pattern/)
  })

  test('built-in kinds take no pattern', () => {
    expect(checkFilterPattern('localhost', undefined)).toBeNull()
    expect(() => checkFilterPattern('web_crawlers', 'Googlebot')).toThrow(/takes no --pattern/)
  })
})

describe('buildFilterUpdate', () => {
  test('sends only what changes', () => {
    expect(buildFilterUpdate({ disable: true })).toEqual({ enabled: false })
    expect(buildFilterUpdate({ pattern: '0.9.*', enable: true })).toEqual({ pattern: '0.9.*', enabled: true })
  })

  test('refuses an empty or contradictory update', () => {
    expect(() => buildFilterUpdate({})).toThrow(/Nothing to change/)
    expect(() => buildFilterUpdate({ enable: true, disable: true })).toThrow(/either --enable or --disable/)
  })
})
