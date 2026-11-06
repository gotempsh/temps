// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  hostSuffixError,
  MAX_HOST_SUFFIX_LEN,
  suffixedHost,
} from '@/lib/tunnel-host-suffix'

describe('tunnel host suffix', () => {
  test('takes one lowercase DNS label fragment', () => {
    expect(hostSuffixError(' Lab ')).toBeUndefined()
    expect(hostSuffixError('lab-2')).toBeUndefined()
    expect(hostSuffixError('')).toBeDefined()
    expect(hostSuffixError('lab.eu')).toBeDefined()
    expect(hostSuffixError('-lab')).toBeDefined()
    expect(hostSuffixError('lab-')).toBeDefined()
    expect(hostSuffixError('lab_1')).toBeDefined()
    expect(hostSuffixError('a'.repeat(MAX_HOST_SUFFIX_LEN))).toBeUndefined()
    expect(hostSuffixError('a'.repeat(MAX_HOST_SUFFIX_LEN + 1))).toBeDefined()
  })

  test('names hosts one level under the domain', () => {
    expect(suffixedHost('shop', 'lab', 'example.com')).toBe(
      'shop-lab.example.com'
    )
    expect(suffixedHost('shop', null, 'example.com')).toBe('shop.example.com')
    expect(suffixedHost('shop', '', 'example.com')).toBe('shop.example.com')
  })
})
