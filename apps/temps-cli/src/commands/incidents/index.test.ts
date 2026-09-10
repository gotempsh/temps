// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  buildBucketedQuery,
  extractIncidentsList,
  incidentStatusBadge,
  isValidIncidentStatus,
  isValidSeverity,
  severityColor,
} from './index.js'

describe('severityColor', () => {
  test('renders known severities distinctly', () => {
    expect(severityColor('critical')).toContain('critical')
    expect(severityColor('major')).toContain('major')
    expect(severityColor('minor')).toContain('minor')
  })

  test('passes an unknown severity through untouched', () => {
    expect(severityColor('weird')).toBe('weird')
  })
})

describe('incidentStatusBadge', () => {
  test('renders resolved as the "active" status badge', () => {
    expect(incidentStatusBadge('resolved')).toContain('active')
  })

  test('renders in-progress statuses using their own label', () => {
    expect(incidentStatusBadge('investigating')).toContain('investigating')
    expect(incidentStatusBadge('identified')).toContain('identified')
    expect(incidentStatusBadge('monitoring')).toContain('monitoring')
  })

  test('passes an unknown status through untouched', () => {
    expect(incidentStatusBadge('weird')).toBe('weird')
  })
})

describe('isValidSeverity', () => {
  test('accepts the known severity levels', () => {
    expect(isValidSeverity('critical')).toBe(true)
    expect(isValidSeverity('minor')).toBe(true)
  })

  test('rejects anything else', () => {
    expect(isValidSeverity('urgent')).toBe(false)
  })
})

describe('isValidIncidentStatus', () => {
  test('accepts the known statuses', () => {
    expect(isValidIncidentStatus('investigating')).toBe(true)
    expect(isValidIncidentStatus('resolved')).toBe(true)
  })

  test('rejects anything else', () => {
    expect(isValidIncidentStatus('closed')).toBe(false)
  })
})

describe('extractIncidentsList', () => {
  test('returns a bare array untouched', () => {
    const data = [{ id: 1 }, { id: 2 }] as unknown as Parameters<typeof extractIncidentsList>[0]
    expect(extractIncidentsList(data)).toHaveLength(2)
  })

  test('unwraps a paginated { data: [...] } envelope', () => {
    const data = { data: [{ id: 2 }] } as unknown as Parameters<typeof extractIncidentsList>[0]
    const result = extractIncidentsList(data)
    expect(result).toHaveLength(1)
    expect(result[0]!.id).toBe(2)
  })

  test('falls back to an empty list rather than throwing on an unexpected shape', () => {
    expect(extractIncidentsList(undefined)).toEqual([])
    expect(extractIncidentsList({} as unknown as Parameters<typeof extractIncidentsList>[0])).toEqual([])
  })
})

describe('buildBucketedQuery', () => {
  test('omits keys for options that were not provided', () => {
    expect(buildBucketedQuery({})).toEqual({})
  })

  test('includes every provided filter under its API field name', () => {
    expect(
      buildBucketedQuery({
        interval: 'daily',
        startTime: '2026-08-01T00:00:00.000Z',
        endTime: '2026-08-08T00:00:00.000Z',
        environmentId: '3',
      }),
    ).toEqual({
      interval: 'daily',
      start_time: '2026-08-01T00:00:00.000Z',
      end_time: '2026-08-08T00:00:00.000Z',
      environment_id: 3,
    })
  })

  test('drops a malformed environment id instead of sending NaN to the API', () => {
    expect(buildBucketedQuery({ environmentId: 'not-a-number' })).toEqual({})
  })
})
