import { test, expect, describe } from 'bun:test'
import { formatAuditUser, formatIpLocation, joinLocationParts, formatAuditData } from './index.js'

describe('formatAuditUser', () => {
  test('prefers the user name when present', () => {
    expect(formatAuditUser({ user: { id: 1, name: 'Ada', email: 'ada@example.com' } })).toBe('Ada')
  })

  test('falls back to email when the name is empty', () => {
    expect(formatAuditUser({ user: { id: 1, name: '', email: 'ada@example.com' } })).toBe(
      'ada@example.com',
    )
  })

  test('falls back to the raw ID when the account has been deleted', () => {
    expect(formatAuditUser({ user: null, user_id: 42 })).toBe('ID: 42')
  })

  test('falls back to the raw ID when user is absent entirely', () => {
    expect(formatAuditUser({ user_id: 7 })).toBe('ID: 7')
  })
})

describe('formatIpLocation', () => {
  test('renders a dash when there is no IP info at all', () => {
    expect(formatIpLocation(null)).toBe('-')
    expect(formatIpLocation(undefined)).toBe('-')
  })

  test('joins city and country', () => {
    expect(formatIpLocation({ ip: '1.2.3.4', city: 'Madrid', country: 'ES' })).toBe('Madrid, ES')
  })

  test('renders city alone when country is missing', () => {
    expect(formatIpLocation({ ip: '1.2.3.4', city: 'Madrid' })).toBe('Madrid')
  })

  test('renders country alone when city is missing', () => {
    expect(formatIpLocation({ ip: '1.2.3.4', country: 'ES' })).toBe('ES')
  })

  test('renders a dash when IP info has neither city nor country', () => {
    expect(formatIpLocation({ ip: '1.2.3.4' })).toBe('-')
  })
})

describe('joinLocationParts', () => {
  test('joins city and country without a fallback dash', () => {
    expect(joinLocationParts({ ip: '1.2.3.4', city: 'Madrid', country: 'ES' })).toBe('Madrid, ES')
  })

  test('returns an empty string when there is nothing to join', () => {
    expect(joinLocationParts({ ip: '1.2.3.4' })).toBe('')
    expect(joinLocationParts(null)).toBe('')
  })
})

describe('formatAuditData', () => {
  test('returns string data untouched, without quoting', () => {
    expect(formatAuditData('plain text')).toBe('plain text')
  })

  test('pretty-prints object data as JSON', () => {
    expect(formatAuditData({ a: 1 })).toBe(JSON.stringify({ a: 1 }, null, 2))
  })

  test('falls back to String() for values JSON.stringify cannot serialise', () => {
    const circular: Record<string, unknown> = {}
    circular.self = circular
    expect(formatAuditData(circular)).toBe(String(circular))
  })
})
