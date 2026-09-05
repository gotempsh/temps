// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  previewGatewayErrorMessage,
  sanitizeGatewayDetail,
} from './preview-gateway-errors'

describe('previewGatewayErrorMessage', () => {
  test('turns a chained Docker bind failure into an actionable port message', () => {
    const message = previewGatewayErrorMessage(
      {
        title: 'Preview gateway error',
        detail:
          'restart failed: failed to start container gateway: Docker daemon rejected the port mapping: address already in use',
      },
      'The preview gateway could not be restarted.',
      18090
    )

    expect(message).toContain('Host port 18090 is already in use')
    expect(message).toContain('Change the configured host port below')
    expect(message).not.toContain('container gateway')
  })

  test('preserves useful non-sensitive RFC 7807 detail', () => {
    expect(
      previewGatewayErrorMessage(
        {
          title: 'Preview gateway error',
          detail: 'failed to pull image: manifest is unknown',
        },
        'Upgrade failed.'
      )
    ).toBe('failed to pull image: manifest is unknown')
  })

  test('does not misclassify registry connectivity and permission failures', () => {
    for (const detail of [
      'failed to connect to registry.example.test: connection refused',
      'registry access denied while pulling the configured image',
    ]) {
      expect(
        previewGatewayErrorMessage(
          { title: 'Preview gateway error', detail },
          'Upgrade failed.'
        )
      ).toBe(detail)
    }
  })
})

describe('sanitizeGatewayDetail', () => {
  test('redacts credentials and query strings from rendered diagnostics', () => {
    const message = sanitizeGatewayDetail(
      'request https://user:pass@example.test/image?token=value failed; Authorization=abc123; PREVIEW_GATEWAY_SHARED_SECRET=super-secret; Bearer token-value'
    )

    expect(message).toContain(
      'https://[redacted]@example.test/image?[redacted]'
    )
    expect(message).toContain('Authorization=[redacted]')
    expect(message).toContain('Bearer [redacted]')
    expect(message).not.toContain('user:pass')
    expect(message).not.toContain('abc123')
    expect(message).not.toContain('super-secret')
    expect(message).not.toContain('token-value')
  })

  test('redacts JSON and scheme-less registry credentials', () => {
    const message = sanitizeGatewayDetail(
      'registry user:pass@example.test denied {"token":"json-value","client_secret":"client-value"}'
    )

    expect(message).toContain('[redacted]@example.test')
    expect(message).toContain('"token":[redacted]')
    expect(message).toContain('"client_secret":[redacted]')
    expect(message).not.toContain('user:pass')
    expect(message).not.toContain('json-value')
    expect(message).not.toContain('client-value')
  })

  test('redacts complete authorization and registry auth header values', () => {
    const message = sanitizeGatewayDetail(
      'Authorization: Basic basic-value; X-Registry-Auth: registry-value; Proxy-Authorization=Token proxy-value'
    )

    expect(message).toContain('Authorization: [redacted]')
    expect(message).toContain('X-Registry-Auth: [redacted]')
    expect(message).toContain('Proxy-Authorization=[redacted]')
    expect(message).not.toContain('basic-value')
    expect(message).not.toContain('registry-value')
    expect(message).not.toContain('proxy-value')
  })
})
