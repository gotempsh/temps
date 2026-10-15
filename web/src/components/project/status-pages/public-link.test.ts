// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  DEFAULT_PUBLIC_URL_SETUP_PATH,
  describePublicLink,
  publicLinkUrl,
} from './public-link'

describe('describePublicLink', () => {
  it('is ready when a URL exists and nothing is wrong with it', () => {
    expect(
      describePublicLink({
        public_url: 'https://temps.example.com/api/status/abc',
        public_url_status: { configured: true, reason: null, setup_path: null },
      })
    ).toEqual({
      kind: 'ready',
      url: 'https://temps.example.com/api/status/abc',
    })
  })

  it('is missing, with the API reason and path, when no URL is configured', () => {
    expect(
      describePublicLink({
        public_url: null,
        public_url_status: {
          configured: false,
          reason: 'Public URL is not configured.',
          setup_path: '/settings#public-ingress',
        },
      })
    ).toEqual({
      kind: 'missing',
      reason: 'Public URL is not configured.',
      setupPath: '/settings#public-ingress',
    })
  })

  it('falls back to a built-in explanation and setting path', () => {
    const state = describePublicLink({})
    expect(state.kind).toBe('missing')
    if (state.kind === 'missing') {
      expect(state.reason).toContain('Public URL is not configured')
      expect(state.setupPath).toBe(DEFAULT_PUBLIC_URL_SETUP_PATH)
    }
  })

  it('treats a blank URL as missing even if the status claims otherwise', () => {
    expect(
      describePublicLink({
        public_url: '   ',
        public_url_status: { configured: true },
      }).kind
    ).toBe('missing')
  })

  it('keeps the URL but warns when it will not open from outside', () => {
    const state = describePublicLink({
      public_url: 'http://localhost:3000/api/status/abc',
      public_url_status: {
        configured: true,
        reason: 'localhost only resolves on the machine itself',
        setup_path: '/settings#public-ingress',
      },
    })
    expect(state).toEqual({
      kind: 'warning',
      url: 'http://localhost:3000/api/status/abc',
      reason: 'localhost only resolves on the machine itself',
      setupPath: '/settings#public-ingress',
    })
  })
})

describe('publicLinkUrl', () => {
  it('never returns a URL for an unconfigured install', () => {
    expect(
      publicLinkUrl({ public_url: null, public_url_status: { configured: false } })
    ).toBeNull()
  })

  it('returns the configured URL', () => {
    expect(
      publicLinkUrl({
        public_url: 'https://temps.example.com/api/status/abc',
      })
    ).toBe('https://temps.example.com/api/status/abc')
  })
})
