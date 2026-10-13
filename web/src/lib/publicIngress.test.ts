// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  publicIngressBaseUrl,
  publicIngressCallbackUrl,
  publicIngressCallbackUrlOrNull,
} from './publicIngress'

describe('publicIngressBaseUrl', () => {
  test('uses the URL the API reports', () => {
    expect(
      publicIngressBaseUrl({ effective_url: 'https://hooks.example.com' })
    ).toBe('https://hooks.example.com')
  })

  test('strips trailing slashes and surrounding whitespace', () => {
    expect(
      publicIngressBaseUrl({ effective_url: '  https://hooks.example.com//  ' })
    ).toBe('https://hooks.example.com')
  })

  test('is null while the status is unavailable, never a guessed origin', () => {
    expect(publicIngressBaseUrl(undefined)).toBeNull()
    expect(publicIngressBaseUrl(null)).toBeNull()
    expect(publicIngressBaseUrl({})).toBeNull()
    expect(publicIngressBaseUrl({ effective_url: '   ' })).toBeNull()
    expect(publicIngressBaseUrl({ effective_url: null })).toBeNull()
  })
})

describe('publicIngressCallbackUrl', () => {
  test('joins a base and an absolute path with exactly one slash', () => {
    expect(
      publicIngressCallbackUrl(
        'https://hooks.example.com',
        '/api/webhook/git/github/events'
      )
    ).toBe('https://hooks.example.com/api/webhook/git/github/events')
    expect(
      publicIngressCallbackUrl(
        'https://hooks.example.com/',
        'api/agents/webhook/wh_1'
      )
    ).toBe('https://hooks.example.com/api/agents/webhook/wh_1')
  })
})

describe('publicIngressCallbackUrlOrNull', () => {
  test('builds the callback URL once the ingress URL is known', () => {
    expect(
      publicIngressCallbackUrlOrNull(
        { effective_url: 'https://hooks.example.com' },
        '/api/webhook/git/github/events'
      )
    ).toBe('https://hooks.example.com/api/webhook/git/github/events')
  })

  test('is null while loading or after a failed read', () => {
    // A registration made with a guessed URL would point the provider at the
    // private console, so there must be nothing to copy or submit.
    expect(
      publicIngressCallbackUrlOrNull(
        undefined,
        '/api/webhook/git/github/events'
      )
    ).toBeNull()
    expect(
      publicIngressCallbackUrlOrNull(
        { effective_url: '' },
        '/api/webhook/git/github/events'
      )
    ).toBeNull()
  })
})
