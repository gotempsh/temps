// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { serviceMetricsEngine } from './service-metrics-engine'

describe('serviceMetricsEngine', () => {
  test('MariaDB services get their own metric layout', () => {
    expect(serviceMetricsEngine('mariadb')).toBe('mariadb')
    expect(serviceMetricsEngine('MariaDB')).toBe('mariadb')
  })

  test('engines with a collector map to themselves', () => {
    for (const engine of [
      'postgres',
      'redis',
      'mongodb',
      's3',
      'rustfs',
    ] as const) {
      expect(serviceMetricsEngine(engine)).toBe(engine)
    }
  })

  test('an s3 service running RustFS uses the RustFS layout', () => {
    expect(serviceMetricsEngine('s3', 'rustfs/rustfs:latest')).toBe('rustfs')
    expect(serviceMetricsEngine('s3', 'minio/minio:latest')).toBe('s3')
  })

  test('service types without a collector are not shown as Postgres', () => {
    for (const engine of ['kv', 'blob', 'minio', 'clickhouse', '']) {
      expect(serviceMetricsEngine(engine)).toBe('unsupported')
    }
  })
})
