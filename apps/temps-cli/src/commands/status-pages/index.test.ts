// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  domainNextStep,
  domainStateLabel,
  parseMonitorSpec,
  parseMonitorSpecs,
  resolveLink,
} from './index.js'

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

describe('resolveLink', () => {
  test('resolves a console-relative link against the instance URL', () => {
    expect(resolveLink('/api/status/abc123', 'https://temps.example.test')).toBe(
      'https://temps.example.test/api/status/abc123',
    )
  })

  test('keeps the port of the instance URL', () => {
    expect(resolveLink('/api/status/abc123/summary.json', 'http://localhost:3000')).toBe(
      'http://localhost:3000/api/status/abc123/summary.json',
    )
  })

  test('an instance URL that already carries /api does not double the prefix', () => {
    expect(resolveLink('/api/status/abc123', 'https://temps.example.test/api')).toBe(
      'https://temps.example.test/api/status/abc123',
    )
  })

  test('leaves an absolute public URL untouched', () => {
    const url = 'https://status.example.test/api/status/abc123'
    expect(resolveLink(url, 'https://temps.example.test')).toBe(url)
  })

  test('returns the relative link when the instance URL cannot be a base', () => {
    expect(resolveLink('/api/status/abc123', 'not a url')).toBe('/api/status/abc123')
  })
})
