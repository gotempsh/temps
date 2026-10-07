// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ON_DEMAND_TLS_SETTINGS_PATH,
  certificatesEmptyStateCopy,
  effectiveZone,
  isLoopbackHost,
  isValidZoneInput,
  normalizeZoneInput,
  onDemandTlsBlockers,
  onDemandTlsState,
} from './on-demand-tls'

describe('onDemandTlsState', () => {
  test('reads the stored switch', () => {
    expect(onDemandTlsState({ on_demand_tls: { enabled: true } }, false)).toBe(
      'enabled'
    )
    expect(onDemandTlsState({ on_demand_tls: { enabled: false } }, false)).toBe(
      'disabled'
    )
    expect(onDemandTlsState({}, false)).toBe('disabled')
  })

  test('a failed or pending read is unknown, never "off"', () => {
    expect(onDemandTlsState(undefined, false)).toBe('unknown')
    expect(onDemandTlsState(undefined, true)).toBe('unknown')
    expect(onDemandTlsState({ on_demand_tls: { enabled: false } }, true)).toBe(
      'unknown'
    )
  })
})

describe('certificatesEmptyStateCopy', () => {
  test('enabled does not tell the user to enable it', () => {
    const copy = certificatesEmptyStateCopy('enabled')
    expect(copy.title).toBe('No certificate attempts yet')
    expect(copy.description).toContain('On-demand TLS is on')
    expect(copy.description.toLowerCase()).not.toContain('enable on-demand')
    expect(copy.action?.href).toBe(ON_DEMAND_TLS_SETTINGS_PATH)
  })

  test('disabled says it is off and links to the switch', () => {
    const copy = certificatesEmptyStateCopy('disabled')
    expect(copy.title).toBe('On-demand TLS is off')
    expect(copy.action).toEqual({
      label: 'Turn on on-demand TLS',
      href: ON_DEMAND_TLS_SETTINGS_PATH,
    })
  })

  test('unknown is neutral and offers no settings link', () => {
    const copy = certificatesEmptyStateCopy('unknown')
    expect(copy.title).toBe('No certificate attempts yet')
    expect(copy.description).not.toContain('is off')
    expect(copy.description).not.toContain('is on.')
    expect(copy.action).toBeNull()
  })
})

describe('effectiveZone', () => {
  test('an explicit zone wins and is normalized', () => {
    expect(effectiveZone(' Apps.Example.COM. ', 'https://x.sslip.io')).toBe(
      'apps.example.com'
    )
  })

  test('a sslip.io external URL is its own zone', () => {
    expect(effectiveZone(null, 'https://1-2-3-4.sslip.io/')).toBe(
      '1-2-3-4.sslip.io'
    )
    expect(effectiveZone('', 'https://203.0.113.7.sslip.io:8443')).toBe(
      '203.0.113.7.sslip.io'
    )
  })

  test('no zone for a custom external URL without an explicit zone', () => {
    expect(effectiveZone(null, 'https://console.example.com')).toBeNull()
    expect(effectiveZone('   ', null)).toBeNull()
  })
})

describe('isLoopbackHost', () => {
  test.each([
    ['localhost', true],
    ['app.localhost', true],
    ['127.0.0.1', true],
    ['127-0-0-1.sslip.io', true],
    ['::1', true],
    ['[::1]', true],
    ['203.0.113.7', false],
    ['console.example.com', false],
  ])('%s -> %p', (host, expected) => {
    expect(isLoopbackHost(host)).toBe(expected)
  })
})

describe('onDemandTlsBlockers', () => {
  test('a ready install has no blockers', () => {
    expect(
      onDemandTlsBlockers(null, {
        external_url: 'https://203-0-113-7.sslip.io',
        letsencrypt: { email: 'ops@example.com' },
      })
    ).toEqual([])
  })

  test('reports loopback, missing zone and missing email with fixes', () => {
    const blockers = onDemandTlsBlockers('', {
      external_url: 'http://localhost:3000',
      letsencrypt: { email: '  ' },
    })
    expect(blockers.map((b) => b.id)).toEqual([
      'loopback',
      'no-zone',
      'no-email',
    ])
    expect(blockers[0].message).toContain('localhost')
    expect(blockers[0].fixHref).toBe('/settings')
    expect(blockers[2].fixHref).toBe('/settings')
  })

  test('an explicit zone clears the zone blocker for a custom domain', () => {
    const blockers = onDemandTlsBlockers('apps.example.com', {
      external_url: 'https://console.example.com',
      letsencrypt: { email: 'ops@example.com' },
    })
    expect(blockers).toEqual([])
  })
})

describe('zone input', () => {
  test('empty means auto-derive', () => {
    expect(normalizeZoneInput('  ')).toBeNull()
    expect(normalizeZoneInput(undefined)).toBeNull()
    expect(isValidZoneInput('')).toBe(true)
  })

  test.each([
    ['apps.example.com', true],
    ['APPS.Example.com.', true],
    ['1-2-3-4.sslip.io', true],
    ['https://apps.example.com', false],
    ['apps.example.com:443', false],
    ['*.apps.example.com', false],
    ['apps..example.com', false],
    ['-apps.example.com', false],
    ['apps.example.com/path', false],
  ])('%s valid=%p', (zone, expected) => {
    expect(isValidZoneInput(zone)).toBe(expected)
  })
})
