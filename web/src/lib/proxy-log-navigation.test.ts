import { describe, expect, test } from 'bun:test'
import { proxyLogDetailUrl } from './proxy-log-navigation'

describe('proxy-log detail navigation', () => {
  test('carries project scope and timestamp for tenant-safe lookups', () => {
    expect(
      proxyLogDetailUrl({
        requestId: 'request/id with spaces',
        timestamp: '2026-08-15T10:00:00.000Z',
        projectId: 42,
      })
    ).toBe(
      '/proxy-logs/request%2Fid%20with%20spaces?ts=2026-08-15T10%3A00%3A00.000Z&project_id=42'
    )
  })
})
