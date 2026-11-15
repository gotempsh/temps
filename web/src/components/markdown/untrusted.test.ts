// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { inlineCodeUrl } from './untrusted'

describe('inlineCodeUrl', () => {
  test('a code span that is one web address is a link', () => {
    expect(inlineCodeUrl(undefined, 'http://app.example.test:8080')).toBe(
      'http://app.example.test:8080'
    )
    expect(inlineCodeUrl(undefined, ['https://app.example.test/a?b=1'])).toBe(
      'https://app.example.test/a?b=1'
    )
  })

  test('anything else stays code', () => {
    expect(inlineCodeUrl(undefined, 'curl http://app.example.test')).toBeNull()
    expect(inlineCodeUrl(undefined, 'ftp://app.example.test')).toBeNull()
    expect(inlineCodeUrl(undefined, 'javascript:alert(1)')).toBeNull()
    expect(inlineCodeUrl(undefined, 'http://a.test\nhttp://b.test')).toBeNull()
    expect(inlineCodeUrl(undefined, 'http://a.test\n')).toBeNull()
    expect(inlineCodeUrl('language-json', 'http://a.test')).toBeNull()
    expect(inlineCodeUrl(undefined, undefined)).toBeNull()
  })
})
