// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  CLOUDFLARE_API_TOKENS_URL,
  CLOUDFLARE_TOKEN_TEMPLATE_URL,
  CloudflareTokenChecklist,
  CloudflareTokenGuide,
  TokenPermissionList,
} from './CloudflareTokenGuide'

describe('CloudflareTokenChecklist', () => {
  test('lists every permission and the resources to limit it to', () => {
    const html = renderToStaticMarkup(<CloudflareTokenChecklist />)
    expect(html).toContain('Zone · Zone · Read')
    expect(html).toContain('Zone · DNS · Edit')
    expect(html).toContain('Account · Cloudflare Tunnel · Edit')
    expect(html).toContain('to create the tunnel and run it on your workers')
    expect(html).toContain('Account Resources')
    expect(html).toContain('Zone Resources')
    expect(html).toContain(CLOUDFLARE_API_TOKENS_URL)
  })

  test('says the tunnel permission covers every tunnel in the account', () => {
    for (const html of [
      renderToStaticMarkup(<CloudflareTokenChecklist />),
      renderToStaticMarkup(<CloudflareTokenGuide tunnel="required" />),
    ]) {
      expect(html).toContain('Covers every tunnel in this Cloudflare account')
      expect(html).toContain('use a separate Cloudflare account for Temps')
      expect(html).toContain('Client IP Address Filtering')
    }
  })

  test('marks the tunnel permission optional where a tunnel is optional', () => {
    const html = renderToStaticMarkup(
      <CloudflareTokenChecklist tunnel="optional" />
    )
    expect(html).toContain(
      'only if workers take traffic through a Cloudflare Tunnel'
    )
    expect(html).not.toContain('to create the tunnel')
  })
})

describe('token template link', () => {
  test('fills in Zone Read and DNS Edit, the same link the server sends', () => {
    expect(CLOUDFLARE_TOKEN_TEMPLATE_URL).toBe(
      'https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys=' +
        '%5B%7B%22key%22%3A%22zone%22%2C%22type%22%3A%22read%22%7D%2C%7B%22key%22%3A%22dns%22%2C' +
        '%22type%22%3A%22edit%22%7D%5D&accountId=*&zoneId=all&name=Temps'
    )
    const keys = JSON.parse(
      new URL(CLOUDFLARE_TOKEN_TEMPLATE_URL).searchParams.get(
        'permissionGroupKeys'
      ) ?? '[]'
    )
    expect(keys).toEqual([
      { key: 'zone', type: 'read' },
      { key: 'dns', type: 'edit' },
    ])
  })
})

describe('TokenPermissionList', () => {
  const result = {
    valid: true,
    zones: ['example.com'],
    checked_zone: 'example.com',
    ready: false,
    template_url: CLOUDFLARE_TOKEN_TEMPLATE_URL,
    permissions: [
      {
        permission: 'zone_read' as const,
        label: 'Zone · Zone · Read',
        state: 'present' as const,
        detail: 'Sees example.com.',
      },
      {
        permission: 'dns_edit' as const,
        label: 'Zone · DNS · Edit',
        state: 'missing' as const,
        detail: 'Add Zone · DNS · Edit for example.com to the token.',
      },
      {
        permission: 'tunnel_edit' as const,
        label: 'Account · Cloudflare Tunnel · Edit',
        state: 'unverified' as const,
        detail: 'Cloudflare did not say.',
      },
    ],
  }

  test('marks each permission present, missing or unverified, with the fix', () => {
    const html = renderToStaticMarkup(<TokenPermissionList result={result} />)
    expect(html).toContain('present')
    expect(html).toContain('missing')
    expect(html).toContain(
      'Add Zone · DNS · Edit for example.com to the token.'
    )
    expect(html).toContain('Account · Cloudflare Tunnel · Edit')
    // Said while the token is checked, before it is saved.
    expect(html).toContain('Covers every tunnel in this Cloudflare account')
    expect(html).not.toContain('Sees example.com.')
  })

  test('leaves the tunnel permission out where no tunnel is made', () => {
    const html = renderToStaticMarkup(
      <TokenPermissionList result={result} tunnel="optional" />
    )
    expect(html).not.toContain('Cloudflare Tunnel')
  })

  test('a token reaching other zones says which and how to narrow it', () => {
    const html = renderToStaticMarkup(
      <TokenPermissionList
        result={{
          ...result,
          scope: {
            zone: 'example.com',
            other_zones: ['example.net'],
            other_count: 1,
            ok: false,
            detail:
              'It also reaches example.net. Limit Zone Resources to example.com.',
          },
        }}
      />
    )
    expect(html).toContain('Reaches more than example.com.')
    expect(html).toContain('It also reaches example.net.')
  })

  test('a rejected token says why', () => {
    const html = renderToStaticMarkup(
      <TokenPermissionList
        result={{
          ...result,
          valid: false,
          error: 'Cloudflare says the token is invalid.',
        }}
      />
    )
    expect(html).toContain('Cloudflare says the token is invalid.')
  })
})
