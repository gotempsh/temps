// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ExternalServiceInfo } from '@/api/client/types.gen'
import { collectAllServicePages } from './useAllServices'

function service(id: number): ExternalServiceInfo {
  return {
    id,
    name: `database-${id}`,
    service_type: 'postgres',
    topology: 'standalone',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    status: 'running',
  }
}

describe('collectAllServicePages', () => {
  test('loads matching databases beyond the first API page', async () => {
    const allServices = Array.from({ length: 205 }, (_, index) =>
      service(index + 1)
    )
    const requestedPages: number[] = []

    const result = await collectAllServicePages(async (page, pageSize) => {
      requestedPages.push(page)
      const start = (page - 1) * pageSize
      return allServices.slice(start, start + pageSize)
    })

    expect(requestedPages).toEqual([1, 2, 3])
    expect(result).toHaveLength(205)
    expect(result[result.length - 1]?.id).toBe(205)
  })

  test('stops after a short first page', async () => {
    let calls = 0
    const result = await collectAllServicePages(async () => {
      calls += 1
      return [service(1)]
    })

    expect(calls).toBe(1)
    expect(result.map((item) => item.id)).toEqual([1])
  })
})
