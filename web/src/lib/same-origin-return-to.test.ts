// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  returnToFromSearch,
  sameOriginReturnTo,
  withReturnTo,
} from './same-origin-return-to'

describe('sameOriginReturnTo', () => {
  it('keeps a same-origin path with its query and hash', () => {
    expect(sameOriginReturnTo('/storage/7')).toBe('/storage/7')
    expect(sameOriginReturnTo('/storage/7?tab=backups#backups')).toBe(
      '/storage/7?tab=backups#backups'
    )
  })

  it('rejects anything that could leave the console', () => {
    for (const bad of [
      null,
      undefined,
      '',
      'storage/7',
      'https://example.com/storage/7',
      '//example.com/storage/7',
      '/\\example.com',
      '/\texample.com',
      '/ storage',
      'javascript:alert(1)',
      '/storage/7\n',
    ]) {
      expect(sameOriginReturnTo(bad)).toBeNull()
    }
  })
})

describe('returnToFromSearch', () => {
  it('reads only a safe returnTo parameter', () => {
    expect(
      returnToFromSearch(new URLSearchParams('returnTo=%2Fstorage%2F7'))
    ).toBe('/storage/7')
    expect(
      returnToFromSearch(new URLSearchParams('returnTo=https://example.com'))
    ).toBeNull()
    expect(returnToFromSearch(new URLSearchParams(''))).toBeNull()
  })
})

describe('withReturnTo', () => {
  it('appends returnTo and keeps existing parameters', () => {
    const href = withReturnTo(
      '/backups/s3-sources/3/schedules/new?service_id=7',
      '/storage/7#backups'
    )
    const url = new URL(href, 'https://temps.invalid')
    expect(url.pathname).toBe('/backups/s3-sources/3/schedules/new')
    expect(url.searchParams.get('service_id')).toBe('7')
    expect(url.searchParams.get('returnTo')).toBe('/storage/7#backups')
  })

  it('drops an unsafe returnTo instead of forwarding it', () => {
    expect(withReturnTo('/backups/s3-sources/new', '//example.com')).toBe(
      '/backups/s3-sources/new'
    )
  })
})
