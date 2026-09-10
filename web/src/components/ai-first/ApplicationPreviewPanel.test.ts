// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { safePreviewHost } from './application-preview'

describe('safePreviewHost', () => {
  test('shows only the preview host and never the authorization grant', () => {
    const url =
      'http://ws-deadbeef-3000.localho.st:8220/__temps/preview/login?grant=1#session_grant=secret'

    expect(safePreviewHost(url)).toBe('ws-deadbeef-3000.localho.st:8220')
    expect(safePreviewHost(url)).not.toContain('secret')
  })

  test('does not render malformed preview URLs', () => {
    expect(safePreviewHost('not a URL')).toBeNull()
    expect(safePreviewHost(null)).toBeNull()
  })
})
