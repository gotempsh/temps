// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { buildBulkFilter, buildBulkRequest, MAX_BULK_ALARM_IDS, parseAlarmIds } from './index.js'

describe('parseAlarmIds', () => {
  test('parses and de-duplicates positive integer IDs', () => {
    expect(parseAlarmIds(['12', '13', '12'])).toEqual([12, 13])
  })

  test('rejects anything that is not a positive integer', () => {
    for (const bad of ['abc', '0', '-3', '1.5', '']) {
      expect(() => parseAlarmIds([bad])).toThrow('Invalid alarm ID')
    }
  })
})

describe('buildBulkFilter', () => {
  test('maps CLI flags onto the API filter fields', () => {
    expect(
      buildBulkFilter({
        status: 'firing',
        severity: 'critical',
        type: 'container_crash',
        environmentId: '4',
        deploymentId: '9',
      }),
    ).toEqual({
      status: 'firing',
      severity: 'critical',
      alarm_type: 'container_crash',
      environment_id: 4,
      deployment_id: 9,
    })
  })

  test('an empty filter matches every active alarm', () => {
    expect(buildBulkFilter({})).toEqual({})
  })

  test('rejects a non-numeric environment ID', () => {
    expect(() => buildBulkFilter({ environmentId: 'prod' })).toThrow('--environment-id')
  })
})

describe('buildBulkRequest', () => {
  test('explicit IDs become alarm_ids', () => {
    expect(buildBulkRequest('resolve', ['3', '4'], {})).toEqual({
      action: 'resolve',
      alarm_ids: [3, 4],
    })
  })

  test('--all becomes a filter request', () => {
    expect(buildBulkRequest('acknowledge', [], { all: true, type: 'high_cpu' })).toEqual({
      action: 'acknowledge',
      filter: { alarm_type: 'high_cpu' },
    })
  })

  test('requires either IDs or --all', () => {
    expect(() => buildBulkRequest('resolve', [], {})).toThrow('--all')
  })

  test('rejects IDs combined with --all', () => {
    expect(() => buildBulkRequest('resolve', ['1'], { all: true })).toThrow('not both')
  })

  test('rejects filters without --all, so they are never silently ignored', () => {
    expect(() => buildBulkRequest('resolve', ['1'], { type: 'container_crash' })).toThrow('only apply with --all')
  })

  test('rejects more IDs than one request may carry', () => {
    const ids = Array.from({ length: MAX_BULK_ALARM_IDS + 1 }, (_, i) => String(i + 1))
    expect(() => buildBulkRequest('resolve', ids, {})).toThrow(`At most ${MAX_BULK_ALARM_IDS}`)
  })
})
