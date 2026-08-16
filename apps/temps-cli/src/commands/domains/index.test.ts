import { test, expect, describe } from 'bun:test'
import { findDomainByName, wildcardBaseDomain, isProvisionedStatus } from './index.js'
import type { DomainResponse } from '../../api/types.gen.js'

function domain(overrides: Partial<DomainResponse>): DomainResponse {
  return {
    id: 1,
    domain: 'example.com',
    status: 'active',
    is_wildcard: false,
    verification_method: 'http-01',
    ...overrides,
  } as DomainResponse
}

describe('findDomainByName', () => {
  test('finds the id of a domain by exact name match', () => {
    const domains = [domain({ id: 1, domain: 'a.com' }), domain({ id: 2, domain: 'b.com' })]
    expect(findDomainByName(domains, 'b.com')).toBe(2)
  })

  test('returns null when no domain matches', () => {
    const domains = [domain({ id: 1, domain: 'a.com' })]
    expect(findDomainByName(domains, 'missing.com')).toBeNull()
  })

  test('returns null for an empty list', () => {
    expect(findDomainByName([], 'a.com')).toBeNull()
  })

  test('is case-sensitive, not a fuzzy match', () => {
    // `domains ssl`/`domains status` resolve id by name before querying
    // status — a loose match here would silently target the wrong domain.
    const domains = [domain({ id: 1, domain: 'Example.com' })]
    expect(findDomainByName(domains, 'example.com')).toBeNull()
  })
})

describe('wildcardBaseDomain', () => {
  test('strips the wildcard prefix', () => {
    expect(wildcardBaseDomain('*.example.com')).toBe('example.com')
  })

  test('leaves a non-wildcard domain untouched', () => {
    expect(wildcardBaseDomain('example.com')).toBe('example.com')
  })

  test('only strips a leading wildcard, not one that appears elsewhere', () => {
    expect(wildcardBaseDomain('sub.*.example.com')).toBe('sub.*.example.com')
  })
})

describe('isProvisionedStatus', () => {
  test('treats active and provisioned as success', () => {
    expect(isProvisionedStatus('active')).toBe(true)
    expect(isProvisionedStatus('provisioned')).toBe(true)
  })

  test('treats every other status as not provisioned', () => {
    expect(isProvisionedStatus('pending')).toBe(false)
    expect(isProvisionedStatus('failed')).toBe(false)
    expect(isProvisionedStatus('')).toBe(false)
  })

  test('is case-sensitive', () => {
    expect(isProvisionedStatus('Active')).toBe(false)
  })
})
