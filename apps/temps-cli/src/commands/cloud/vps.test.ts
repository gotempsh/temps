import { test, expect, describe } from 'bun:test'
import { formatPrice, vpsStatusBadge } from './vps.js'

describe('formatPrice', () => {
  test('renders zero cents as free', () => {
    expect(formatPrice(0)).toContain('free')
  })

  test('converts cents to euros with two decimals', () => {
    expect(formatPrice(499)).toContain('€4.99/mo')
  })
})

describe('vpsStatusBadge', () => {
  test('includes the raw status text for every known status', () => {
    for (const status of [
      'provisioning',
      'installing',
      'stalling',
      'destroying',
      'active',
      'error',
      'destroyed',
    ]) {
      expect(vpsStatusBadge(status)).toContain(status)
    }
  })

  test('matches status case-insensitively', () => {
    expect(vpsStatusBadge('ACTIVE')).toContain('ACTIVE')
  })

  test('falls back to a default color for an unknown status rather than throwing', () => {
    expect(() => vpsStatusBadge('unknown-status')).not.toThrow()
    expect(vpsStatusBadge('unknown-status')).toContain('unknown-status')
  })

  test('prefixes the status with a bullet marker', () => {
    expect(vpsStatusBadge('active')).toContain('● active')
  })
})
