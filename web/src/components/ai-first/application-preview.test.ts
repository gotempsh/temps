// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'
import {
  previewAccessRequest,
  previewErrorMessage,
  previewCookieErrorMessage,
  previewRenewalPath,
  safePreviewPath,
} from './application-preview'

describe('preview authorization', () => {
  test('explains HTTP iframe cookies without exposing the preview grant', () => {
    const message = previewCookieErrorMessage(
      'http://ws-example.preview.test:8080/#grant=private-value'
    )
    expect(message).toContain('HTTP')
    expect(message).toContain('cross-site iframe')
    expect(message).toContain('new tab')
    expect(message).toContain('configure HTTPS')
    expect(message).not.toContain('private-value')
    expect(message).not.toContain('server is down')
  })
  test('does not misdiagnose HTTPS cookie restrictions as HTTP', () => {
    for (const url of [
      'https://preview.test/#grant=private-value',
      'invalid',
    ]) {
      const message = previewCookieErrorMessage(url)
      expect(message).toContain('browser cookie restrictions')
      expect(message).not.toContain('uses HTTP')
      expect(message).not.toContain('private-value')
    }
  })
  test('renews path-free notifications without guessing a new destination', () => {
    expect(
      previewRenewalPath({ type: 'temps:preview-auth-required' }, '/known')
    ).toBe('/known')
    expect(
      previewRenewalPath(
        { type: 'temps:preview-auth-required', path: '/nested?q=1' },
        '/'
      )
    ).toBe('/nested?q=1')
    expect(
      previewRenewalPath(
        { type: 'temps:preview-auth-required', path: '//evil.test' },
        '/'
      )
    ).toBeNull()
    expect(previewRenewalPath({ type: 'other' }, '/')).toBeNull()
    expect(previewRenewalPath(null, '/')).toBeNull()
  })
  test('preserves a local path and query without accepting a destination origin', () => {
    expect(
      previewAccessRequest(
        new URLSearchParams({
          sandbox: 'sbx_0123456789abcdef',
          port: '3000',
          path: '/dashboard?q=hello',
        })
      )
    ).toEqual({
      sandbox_public_id: 'sbx_0123456789abcdef',
      port: 3000,
      path: '/dashboard?q=hello',
    })
  })
  test('rejects invalid sandbox identifiers and ports', () => {
    for (const sandbox of ['', '../foo', 'sbx_nothex'])
      expect(
        previewAccessRequest(new URLSearchParams({ sandbox, port: '3000' }))
      ).toBeNull()
    for (const port of ['', '0', '65536', '3.5', 'NaN'])
      expect(
        previewAccessRequest(
          new URLSearchParams({ sandbox: 'sbx_0123456789abcdef', port })
        )
      ).toBeNull()
  })
  test('rejects external redirects, backslashes and control characters', () => {
    for (const path of [
      'https://evil.test',
      '//evil.test',
      '/\\evil.test',
      '/\nfoo',
      '/' + 'a'.repeat(8192),
      null,
      {},
    ])
      expect(safePreviewPath(path)).toBe(false)
    expect(safePreviewPath('/nested/path?tab=1')).toBe(true)
  })
  test('keeps underlying API errors instead of claiming the app is down', () => {
    expect(
      previewErrorMessage({
        detail: 'Workspace access was revoked.',
        title: 'Forbidden',
      })
    ).toBe('Workspace access was revoked.')
    expect(previewErrorMessage(new Error('Network unavailable'))).toBe(
      'Network unavailable'
    )
    expect(previewErrorMessage(null)).toBe(
      'Temps could not open this sandbox preview.'
    )
  })
})
