// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildSavedQuery, validateSeriesMetric } from './saved.js'

describe('buildSavedQuery', () => {
  test('an empty set of flags matches every collected line', () => {
    expect(buildSavedQuery({})).toEqual({
      source: 'collected',
      levels: [],
      envs: [],
      services: [],
      container_ids: [],
      node_ids: [],
      external_service_ids: [],
      deploy_id: null,
      text: null,
      attrs: [],
    })
  })

  test('reads every filter flag', () => {
    const query = buildSavedQuery({
      source: 'application',
      level: ['error', 'WARN'],
      env: ['production'],
      service: ['checkout'],
      node: ['2'],
      deploy: '11',
      text: '  payment failed ',
      attr: ['status=500'],
    })
    expect(query.levels).toEqual(['ERROR', 'WARN'])
    expect(query.node_ids).toEqual([2])
    expect(query.deploy_id).toBe(11)
    expect(query.text).toBe('payment failed')
    expect(query.attrs).toEqual(['status=500'])
    expect(query.source).toBe('application')
  })

  test('refuses an unknown level, source or id', () => {
    expect(() => buildSavedQuery({ level: ['loud'] })).toThrow('--level')
    expect(() => buildSavedQuery({ source: 'syslog' })).toThrow('--source')
    expect(() => buildSavedQuery({ node: ['0'] })).toThrow('--node')
    expect(() => buildSavedQuery({ deploy: 'x' })).toThrow('--deploy')
  })
})

describe('validateSeriesMetric', () => {
  test('accepts a count or a function of an attribute', () => {
    expect(validateSeriesMetric('count')).toBe('count')
    expect(validateSeriesMetric('p95:duration_ms')).toBe('p95:duration_ms')
  })

  test('refuses anything else', () => {
    expect(() => validateSeriesMetric('median:x')).toThrow('--metric')
    expect(() => validateSeriesMetric('avg:')).toThrow('--metric')
  })
})
