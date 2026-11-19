// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  domainNextStep,
  domainStateLabel,
  applyGroupSpecs,
  parseMonitorSpec,
  parseMonitorSpecs,
  parseSelection,
  publicUrlLabel,
  publicUrlNote,
} from './index.js'

describe('public URL display', () => {
  const unconfigured = {
    public_url: null,
    public_url_status: {
      configured: false,
      reason: 'Public URL is not configured.',
      setup_path: '/settings#public-ingress',
    },
  }

  test('prints the URL when there is one', () => {
    expect(
      publicUrlLabel({
        public_url: 'https://temps.example.com/api/status/abc',
        public_url_status: { configured: true },
      }),
    ).toBe('https://temps.example.com/api/status/abc')
  })

  test('says "not configured" instead of printing a made-up address', () => {
    expect(publicUrlLabel(unconfigured)).toBe('not configured')
    expect(publicUrlLabel({})).toBe('not configured')
  })

  test('explains an unconfigured URL and where to fix it', () => {
    const note = publicUrlNote(unconfigured)
    expect(note).toContain('Public URL is not configured.')
    expect(note).toContain('/settings#public-ingress')
  })

  test('has nothing to say when the URL is fine', () => {
    expect(
      publicUrlNote({
        public_url: 'https://temps.example.com/api/status/abc',
        public_url_status: { configured: true, reason: null, setup_path: null },
      }),
    ).toBeNull()
  })
})

describe('parseMonitorSpec', () => {
  test('parses <id>=<public name>', () => {
    expect(parseMonitorSpec('12=Public API')).toEqual({
      monitor_id: 12,
      display_name: 'Public API',
    })
  })

  test('keeps "=" inside the public name', () => {
    expect(parseMonitorSpec('3=a=b')).toEqual({ monitor_id: 3, display_name: 'a=b' })
  })

  test('rejects a missing name, bad id or missing separator', () => {
    expect(typeof parseMonitorSpec('12')).toBe('string')
    expect(typeof parseMonitorSpec('12=  ')).toBe('string')
    expect(typeof parseMonitorSpec('abc=API')).toBe('string')
    expect(typeof parseMonitorSpec('-1=API')).toBe('string')
    expect(typeof parseMonitorSpec('=API')).toBe('string')
  })
})

describe('parseMonitorSpecs', () => {
  test('keeps order and stops at the first invalid spec', () => {
    expect(parseMonitorSpecs(['2=Web', '1=API'])).toEqual([
      { monitor_id: 2, display_name: 'Web' },
      { monitor_id: 1, display_name: 'API' },
    ])
    expect(typeof parseMonitorSpecs(['2=Web', 'bad'])).toBe('string')
    expect(parseMonitorSpecs(undefined)).toEqual([])
  })
})

describe('applyGroupSpecs / parseSelection', () => {
  test('groups selected monitors and leaves the rest ungrouped', () => {
    expect(parseSelection(['12=API', '13=Dashboard', '20=Email'], ['Cloud=12, 13'])).toEqual([
      { monitor_id: 12, display_name: 'API', group_name: 'Cloud' },
      { monitor_id: 13, display_name: 'Dashboard', group_name: 'Cloud' },
      { monitor_id: 20, display_name: 'Email' },
    ])
  })

  test('keeps "=" inside the group name', () => {
    expect(applyGroupSpecs([{ monitor_id: 1, display_name: 'A' }], ['a=b=1'])).toEqual([
      { monitor_id: 1, display_name: 'A', group_name: 'a=b' },
    ])
  })

  test('rejects unknown or doubly grouped monitors and malformed specs', () => {
    const selection = [{ monitor_id: 1, display_name: 'A' }]
    expect(typeof applyGroupSpecs(selection, ['Cloud=2'])).toBe('string')
    expect(typeof applyGroupSpecs(selection, ['Cloud=1', 'Edge=1'])).toBe('string')
    expect(typeof applyGroupSpecs(selection, ['=1'])).toBe('string')
    expect(typeof applyGroupSpecs(selection, ['Cloud'])).toBe('string')
    expect(typeof applyGroupSpecs(selection, ['Cloud=x'])).toBe('string')
    expect(applyGroupSpecs(selection, ['Cloud=1', 'Cloud=1'])).toEqual([
      { monitor_id: 1, display_name: 'A', group_name: 'Cloud' },
    ])
  })
})

describe('domainStateLabel', () => {
  test('describes each custom domain state', () => {
    expect(domainStateLabel({ status: 'active', serving: true })).toBe('serving')
    expect(domainStateLabel({ status: 'active', serving: false })).toContain('unpublished')
    expect(domainStateLabel({ status: 'pending', serving: false })).toBe('waiting for DNS')
    expect(domainStateLabel({ status: 'unverified', serving: false })).toBe('needs verification')
    expect(domainStateLabel({ status: 'failed', serving: false })).toBe('verification failed')
    expect(domainStateLabel({ status: 'renewal_pending', serving: true })).toContain('renewal in progress')
    expect(domainStateLabel({ status: 'renewal_failed', serving: true })).toContain('renewal failed')
  })
})

describe('domainNextStep', () => {
  test('points DNS-01 certificates at renew, since they are not renewed automatically', () => {
    expect(domainNextStep({ status: 'active', verification_method: 'dns-01' })).toContain('domains renew')
    expect(domainNextStep({ status: 'active', verification_method: 'http-01' })).toBeNull()
  })

  test('a pending renewal is finished with verify and a failed one retried with renew', () => {
    expect(domainNextStep({ status: 'renewal_pending', verification_method: 'dns-01' })).toContain('TXT')
    expect(domainNextStep({ status: 'renewal_pending', verification_method: 'dns-01' })).toContain('domains verify')
    expect(domainNextStep({ status: 'renewal_failed', verification_method: 'http-01' })).toContain('domains renew')
    expect(domainNextStep({ status: 'pending', verification_method: 'http-01' })).toContain('domains verify')
  })
})
