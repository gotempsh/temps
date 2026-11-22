// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  providerFromAddParam,
  providerKeyEndpoint,
} from './ai-providers-shared'

describe('providerFromAddParam', () => {
  test('resolves a supported provider id from the deep link', () => {
    expect(providerFromAddParam('openrouter')?.name).toBe('OpenRouter')
  })

  test('ignores missing, unknown and inherited keys', () => {
    expect(providerFromAddParam(null)).toBeUndefined()
    expect(providerFromAddParam('')).toBeUndefined()
    expect(providerFromAddParam('not-a-provider')).toBeUndefined()
    expect(providerFromAddParam('constructor')).toBeUndefined()
    expect(providerFromAddParam('__proto__')).toBeUndefined()
  })
})

describe('providerKeyEndpoint', () => {
  test('is the host and port of a custom base URL', () => {
    expect(providerKeyEndpoint('https://api.example.com/v1')).toBe(
      'api.example.com'
    )
    expect(providerKeyEndpoint(' http://10.0.0.5:8000/v1 ')).toBe(
      '10.0.0.5:8000'
    )
  })

  test('is null for the vendor default endpoint', () => {
    expect(providerKeyEndpoint(null)).toBeNull()
    expect(providerKeyEndpoint(undefined)).toBeNull()
    expect(providerKeyEndpoint('   ')).toBeNull()
  })

  test('falls back to the raw value when it is not a URL', () => {
    expect(providerKeyEndpoint('not a url')).toBe('not a url')
  })
})
