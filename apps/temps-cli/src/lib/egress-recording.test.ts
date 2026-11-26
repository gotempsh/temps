// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { EgressActivityRecord } from '../api/types.gen.js'
import { describeDestination, parseOutcomeFilter, recordingChange } from './egress-recording.js'

const record = (overrides: Partial<EgressActivityRecord>): EgressActivityRecord =>
  ({
    bytes_received: 0,
    bytes_sent: 0,
    count: 1,
    duration_ms: 1,
    kind: 'tunnel',
    outcome: 'allowed',
    timestamp: '2026-01-01T00:00:00Z',
    ...overrides,
  }) as EgressActivityRecord

describe('egress recording flags', () => {
  test('--outcome takes the API values, case-insensitively', () => {
    expect(parseOutcomeFilter(undefined)).toBeUndefined()
    expect(parseOutcomeFilter('Refused')).toBe('refused')
    expect(() => parseOutcomeFilter('denied')).toThrow(/allowed, refused, failed/)
  })

  test('--on and --off ask for a change, never both', () => {
    expect(recordingChange({})).toBeUndefined()
    expect(recordingChange({ on: true })).toBe(true)
    expect(recordingChange({ off: true })).toBe(false)
    expect(() => recordingChange({ on: true, off: true })).toThrow(/cannot be combined/)
  })
})

describe('describeDestination', () => {
  test('a tunnel is its host and port, a forward its method and path, a relay its kind', () => {
    expect(describeDestination(record({ host: 'api.example.com', port: 443 }))).toBe('api.example.com:443')
    expect(
      describeDestination(record({ host: 'api.example.com', port: 80, method: 'GET', path: '/v1' })),
    ).toBe('GET api.example.com:80/v1')
    expect(describeDestination(record({ relay: 'model' }))).toBe('model relay')
    expect(describeDestination(record({}))).toBe('(no destination)')
  })
})
