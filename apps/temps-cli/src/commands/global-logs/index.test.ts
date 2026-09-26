// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  buildFilters,
  describeCollection,
  facetValueName,
  parseDuration,
  toAnalyticsQuery,
  validateAttrPredicate,
  validateGroupKey,
  validateGroupKeys,
  validateMetric,
} from './index.js'

describe('parseDuration', () => {
  test('accepts minutes, hours and days', () => {
    expect(parseDuration('30m')).toBe(30 * 60_000)
    expect(parseDuration('6h')).toBe(6 * 3_600_000)
    expect(parseDuration('7d')).toBe(7 * 86_400_000)
  })

  test('refuses anything it cannot interpret instead of guessing a window', () => {
    for (const bad of ['', '6', 'h', '6w', '-1h', 'six hours'])
      expect(() => parseDuration(bad)).toThrow(/Invalid duration/)
  })
})

describe('buildFilters', () => {
  test('defaults to the last hour ending now', () => {
    const filters = buildFilters({})
    const span =
      Date.parse(filters.end_time) - Date.parse(filters.start_time)
    expect(span).toBe(3_600_000)
    expect(filters.source).toBe('collected')
  })

  test('maps every repeatable flag onto its request field', () => {
    const filters = buildFilters({
      startTime: '2026-09-09T00:00:00Z',
      endTime: '2026-09-09T12:00:00Z',
      source: 'application',
      project: ['storefront', '4'],
      externalService: ['orders-db'],
      scope: ['application:12'],
      level: ['error', 'Warn'],
      env: ['production'],
      service: ['web'],
      container: ['container-1'],
      node: ['7'],
      deploy: '42',
      text: 'timeout',
    })
    expect(filters).toMatchObject({
      start_time: '2026-09-09T00:00:00.000Z',
      end_time: '2026-09-09T12:00:00.000Z',
      source: 'application',
      projects: ['storefront', '4'],
      external_services: ['orders-db'],
      scopes: ['application:12'],
      // Levels are normalized to the wire casing rather than rejected.
      levels: ['ERROR', 'WARN'],
      envs: ['production'],
      services: ['web'],
      container_ids: ['container-1'],
      node_ids: [7],
      deploy_id: 42,
      text: 'timeout',
    })
  })

  test('rejects bad input at the CLI rather than sending it to the server', () => {
    expect(() => buildFilters({ source: 'everything' })).toThrow(/--source/)
    expect(() => buildFilters({ level: ['critical'] })).toThrow(/--level/)
    expect(() => buildFilters({ node: ['worker-7'] })).toThrow(/--node/)
    expect(() => buildFilters({ deploy: '0' })).toThrow(/--deploy/)
    expect(() => buildFilters({ endTime: 'yesterday' })).toThrow(/--end-time/)
    expect(() =>
      buildFilters({
        startTime: '2026-09-09T12:00:00Z',
        endTime: '2026-09-09T00:00:00Z',
      })
    ).toThrow(/before its end/)
  })

  test('omits optional fields entirely when unset, so the server default applies', () => {
    const filters = buildFilters({})
    expect('deploy_id' in filters).toBe(false)
    expect('text' in filters).toBe(false)
  })

  test('maps --attr predicates onto attrs, validating each one', () => {
    const filters = buildFilters({
      attr: ['status_code=200', 'http_route^=/api/', 'duration_ms>500', 'worker?'],
    })
    expect(filters.attrs).toEqual([
      'status_code=200',
      'http_route^=/api/',
      'duration_ms>500',
      'worker?',
    ])
  })

  test('rejects a malformed --attr at the CLI', () => {
    expect(() => buildFilters({ attr: ['no-operator'] })).toThrow(/--attr/)
    expect(() => buildFilters({ attr: ['=novalue'] })).toThrow(/--attr/)
    expect(() => buildFilters({ attr: ['?'] })).toThrow(/--attr/)
  })

  test('defaults attrs to an empty array when unset', () => {
    expect(buildFilters({}).attrs).toEqual([])
  })
})

describe('validateAttrPredicate', () => {
  test('accepts every operator the server understands', () => {
    for (const pred of [
      'status_code=200',
      'status_code!=200',
      'http_route^=/api/',
      'duration_ms>500',
      'duration_ms<10',
      'worker?',
    ]) {
      expect(validateAttrPredicate(pred)).toBe(pred)
    }
  })

  test('rejects predicates with no key or no operator', () => {
    for (const bad of ['no-operator-here', '=200', '!=200', '^=/api/', '>500', '<10', '?'])
      expect(() => validateAttrPredicate(bad)).toThrow(/Invalid --attr/)
  })
})

describe('validateGroupKey / validateGroupKeys', () => {
  test('accepts label names and attr:<name>', () => {
    for (const key of ['env', 'service', 'level', 'stream', 'project', 'external_service', 'node', 'deploy', 'container', 'attr:worker'])
      expect(validateGroupKey(key)).toBe(key)
  })

  test('rejects unknown labels and an empty attr: name', () => {
    expect(() => validateGroupKey('bogus')).toThrow(/Invalid group key/)
    expect(() => validateGroupKey('attr:')).toThrow(/needs a name/)
  })

  test('splits, trims and validates a comma-separated list', () => {
    expect(validateGroupKeys(' env , attr:worker ,service')).toBe('env,attr:worker,service')
  })

  test('rejects an empty group-by list', () => {
    expect(() => validateGroupKeys(' , ,')).toThrow(/at least one/i)
  })
})

describe('validateMetric', () => {
  test('accepts count and every <fn>:<attr> form', () => {
    expect(validateMetric('count')).toBe('count')
    for (const metric of [
      'count_distinct:user_id',
      'avg:duration_ms',
      'p50:duration_ms',
      'p95:duration_ms',
      'p99:duration_ms',
      'max:duration_ms',
      'sum:bytes',
    ]) {
      expect(validateMetric(metric)).toBe(metric)
    }
  })

  test('rejects unknown functions, missing attrs, and stray input', () => {
    for (const bad of ['bogus:x', 'avg:', ':x', 'count:x', 'AVG:x'])
      expect(() => validateMetric(bad)).toThrow(/Invalid --metric/)
  })
})

describe('toAnalyticsQuery', () => {
  test('strips text, cursor, page_size and attrs; keeps the shared filter fields', () => {
    const filters = buildFilters({
      startTime: '2026-09-09T00:00:00Z',
      endTime: '2026-09-09T12:00:00Z',
      env: ['production'],
      text: 'timeout',
      attr: ['worker?'],
    })
    const query = toAnalyticsQuery({ ...filters, cursor: 'abc', page_size: 200 })
    expect(query).toMatchObject({
      start_time: '2026-09-09T00:00:00.000Z',
      end_time: '2026-09-09T12:00:00.000Z',
      envs: ['production'],
    })
    expect('text' in query).toBe(false)
    expect('cursor' in query).toBe(false)
    expect('page_size' in query).toBe(false)
    expect('attrs' in query).toBe(false)
  })
})

describe('describeCollection', () => {
  const base = {
    collecting: true,
    details_visible: true,
    deferred_count: 0,
    deferred_bytes: 0,
    deferred: [],
  }

  test('a running instance with nothing deferred is fine', () => {
    const summary = describeCollection({ ...base, state: 'running' })
    expect(summary.severity).toBe('ok')
    expect(summary.details).toEqual([])
  })

  test('an unreadable deferred directory is a warning, not a clean bill of health', () => {
    const summary = describeCollection({
      ...base,
      state: 'running',
      deferred_error: 'IO error: Permission denied',
    })
    expect(summary.severity).toBe('warn')
    expect(summary.details).toEqual([
      'Could not check for WAL generations set aside by recovery: IO error: Permission denied',
    ])
  })

  test('a paused instance says why and when it retries', () => {
    const summary = describeCollection({
      ...base,
      state: 'retrying',
      collecting: false,
      error: 'object storage unreachable',
      retry_at: '2026-01-01T00:10:00Z',
    })
    expect(summary.severity).toBe('error')
    expect(summary.headline).toContain('paused')
    expect(summary.details).toEqual([
      'Error: object storage unreachable',
      'Next attempt: 2026-01-01T00:10:00Z',
    ])
  })

  test('deferred generations warn even while collecting, and name the overflow', () => {
    const summary = describeCollection({
      ...base,
      state: 'running',
      deferred_count: 3,
      deferred_bytes: 3 * 1024 * 1024,
      deferred_dir: '/data/logs/wal/deferred',
      deferred: [{ file_name: 'a.b.sealed-wal', bytes: 1, reason: 'record fails its checksum' }],
    })
    expect(summary.severity).toBe('warn')
    expect(summary.details[0]).toContain('3 WAL generation(s) (3.0 MiB)')
    expect(summary.details[0]).toContain('/data/logs/wal/deferred')
    expect(summary.details[1]).toBe('  a.b.sealed-wal: record fails its checksum')
    expect(summary.details[2]).toBe('  …and 2 more')
  })
})

test('describeCollection tells non-administrators where the details are', () => {
  const summary = describeCollection({
    state: 'stopped',
    collecting: false,
    details_visible: false,
    deferred_count: 0,
    deferred_bytes: 0,
    deferred: [],
  })
  expect(summary.severity).toBe('error')
  expect(summary.details).toEqual([
    'An instance administrator can see the exact error and file locations.',
  ])
})

describe('facetValueName', () => {
  const result = {
    project_names: { '3': 'storefront' },
    external_service_names: { '5': 'orders-db' },
  }

  test('names project and service ids the server resolved', () => {
    expect(facetValueName(result, 'project_id', '3')).toBe('storefront')
    expect(facetValueName(result, 'external_service_id', '5')).toBe('orders-db')
  })

  test('says an unnamed id no longer exists instead of printing nothing', () => {
    expect(facetValueName(result, 'project_id', '9')).toBe('unknown (no longer exists)')
  })

  test('project 0 is a database service, not a deleted project', () => {
    expect(facetValueName(result, 'project_id', '0')).toBe('none (database service)')
  })

  test('leaves readable fields, and servers that send no names, alone', () => {
    expect(facetValueName(result, 'env', 'production')).toBeUndefined()
    expect(facetValueName({}, 'project_id', '3')).toBeUndefined()
  })
})
