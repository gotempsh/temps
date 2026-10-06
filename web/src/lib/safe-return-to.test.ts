// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isReturnFromSetup,
  locationPath,
  returnNavigation,
  returnToFromSearch,
  safeReturnTo,
  withReturnTo,
} from './safe-return-to'

describe('safeReturnTo', () => {
  test('accepts in-app paths with query and hash', () => {
    expect(safeReturnTo('/projects/demo/errors/alert-rules/new')).toBe(
      '/projects/demo/errors/alert-rules/new'
    )
    expect(safeReturnTo('/monitoring/rules?tab=x#top')).toBe(
      '/monitoring/rules?tab=x#top'
    )
  })

  test.each([
    null,
    undefined,
    '',
    'projects/demo',
    'https://example.com/',
    '//example.com/path',
    '/\\example.com',
    '/path with space',
    '/tab\tnewline',
    'javascript:alert(1)',
    '/storage/7\n',
    '/ storage',
  ])('rejects %p', (value) => {
    expect(safeReturnTo(value)).toBeNull()
  })
})

describe('withReturnTo', () => {
  test('appends an encoded returnTo and keeps existing query params', () => {
    const href = withReturnTo(
      '/settings/notifications?tab=routes',
      '/projects/demo/metrics/alerts/new?x=1'
    )
    const url = new URL(href, 'https://temps.invalid')
    expect(url.pathname).toBe('/settings/notifications')
    expect(url.searchParams.get('tab')).toBe('routes')
    expect(url.searchParams.get('returnTo')).toBe(
      '/projects/demo/metrics/alerts/new?x=1'
    )
  })

  test('keeps a hash on returnTo and existing params on the target', () => {
    const href = withReturnTo(
      '/backups/s3-sources/3/schedules/new?service_id=7',
      '/storage/7#backups'
    )
    const url = new URL(href, 'https://temps.invalid')
    expect(url.pathname).toBe('/backups/s3-sources/3/schedules/new')
    expect(url.searchParams.get('service_id')).toBe('7')
    expect(url.searchParams.get('returnTo')).toBe('/storage/7#backups')
  })

  test('drops an unsafe returnTo', () => {
    expect(
      withReturnTo('/settings/notifications/new', 'https://example.com')
    ).toBe('/settings/notifications/new')
  })
})

describe('returnToFromSearch', () => {
  test('reads only a safe returnTo parameter', () => {
    expect(
      returnToFromSearch(new URLSearchParams('returnTo=%2Fstorage%2F7'))
    ).toBe('/storage/7')
    expect(
      returnToFromSearch(new URLSearchParams('returnTo=https://example.com'))
    ).toBeNull()
    expect(
      returnToFromSearch(new URLSearchParams('returnTo=%2F%2Fexample.com'))
    ).toBeNull()
    expect(returnToFromSearch(new URLSearchParams(''))).toBeNull()
  })
})

test('locationPath joins pathname, search and hash', () => {
  expect(locationPath({ pathname: '/a', search: '?b=1', hash: '#c' })).toBe(
    '/a?b=1#c'
  )
})

test('return navigation is recognisable by the task page', () => {
  expect(isReturnFromSetup(returnNavigation().state)).toBe(true)
  expect(returnNavigation().replace).toBe(true)
  expect(isReturnFromSetup(null)).toBe(false)
  expect(isReturnFromSetup({ returnedFromSetup: 'yes' })).toBe(false)
})
