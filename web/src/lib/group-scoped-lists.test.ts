// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import type {
  CustomDomainResponse,
  DeploymentResponse,
  ProjectServiceInfo,
  ServiceBackupEntryResponse,
} from '@/api/client/types.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  BACKUPS_PER_DATABASE,
  DEPLOYMENTS_PER_SERVICE,
  GROUP_FANOUT_LIMIT,
  fanoutMembers,
  groupBackupQueries,
  groupDatabaseQueries,
  groupDeploymentQueries,
  groupDomainQueries,
  mergeGroupBackups,
  mergeGroupDatabases,
  mergeGroupDeployments,
  mergeGroupDomains,
} from './group-scoped-lists'

const web = { id: 11, name: 'web', slug: 'web' }
const api = { id: 12, name: 'api', slug: 'api' }

/**
 * Runs each query's request and returns the requests sent. The queries are
 * built after the API mock is installed, since a query key captures the
 * client's base URL.
 */
async function requested(
  build: () => readonly { queryKey: unknown; queryFn?: unknown }[]
) {
  const mock = mockApi({})
  const urls: string[] = []
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const original = mock.calls
  for (const query of build()) {
    await client
      .fetchQuery(query as Parameters<QueryClient['fetchQuery']>[0])
      .catch(() => undefined)
  }
  for (const call of original) urls.push(`${call.method} ${call.path}`)
  mock.restore()
  return urls
}

describe('scoped list requests', () => {
  test('databases come from each service, never the server-wide list', async () => {
    const urls = await requested(() => groupDatabaseQueries([web, api]))
    expect(urls).toEqual([
      'GET /external-services/projects/11',
      'GET /external-services/projects/12',
    ])
    expect(urls).not.toContain('GET /external-services')
  })

  test('domains come from each service, never the server-wide list', async () => {
    const urls = await requested(() => groupDomainQueries([web, api]))
    expect(urls).toEqual([
      'GET /projects/11/custom-domains',
      'GET /projects/12/custom-domains',
    ])
    expect(urls).not.toContain('GET /domains')
  })

  test('deployments come from each service, a bounded page each', async () => {
    const queries = groupDeploymentQueries([web])
    expect(queries[0].queryKey[0].query).toEqual({
      page: 1,
      per_page: DEPLOYMENTS_PER_SERVICE,
    })
    expect(await requested(() => groupDeploymentQueries([web]))).toEqual([
      'GET /projects/11/deployments',
    ])
  })

  test('backups come from each of the project databases', async () => {
    const queries = groupBackupQueries([7, 8])
    expect(queries[0].queryKey[0].query).toEqual({
      page: 1,
      page_size: BACKUPS_PER_DATABASE,
    })
    const urls = await requested(() => groupBackupQueries([7, 8]))
    expect(urls).toEqual([
      'GET /backups/external-services/7/backups',
      'GET /backups/external-services/8/backups',
    ])
    expect(urls).not.toContain('GET /backups/schedules')
  })

  test('fan-out is capped and says so', () => {
    const members = Array.from({ length: GROUP_FANOUT_LIMIT + 3 }, (_, i) => ({
      id: i + 1,
      name: `s${i}`,
      slug: `s${i}`,
    }))
    expect(fanoutMembers(members).requested).toHaveLength(GROUP_FANOUT_LIMIT)
    expect(fanoutMembers(members).truncated).toBe(true)
    expect(groupDatabaseQueries(members)).toHaveLength(GROUP_FANOUT_LIMIT)
    expect(fanoutMembers([web]).truncated).toBe(false)
  })

  test('no services means no requests', async () => {
    expect(groupDatabaseQueries([])).toEqual([])
    expect(await requested(() => groupDomainQueries([]))).toEqual([])
  })

  test('a request is answered with the service data', async () => {
    const mock = mockApi({
      'GET /projects/11/custom-domains': () =>
        jsonResponse({ domains: [], total: 0 }),
    })
    const client = new QueryClient()
    const data = await client.fetchQuery(groupDomainQueries([web])[0])
    expect(data).toEqual({ domains: [], total: 0 })
    mock.restore()
  })
})

const link = (id: number, name: string) =>
  ({
    id: id * 100,
    service: { id, name, service_type: 'redis', status: 'running' },
  }) as unknown as ProjectServiceInfo

describe('merging', () => {
  test('a database shared by two services is listed once with both', () => {
    const merged = mergeGroupDatabases(
      [web, api],
      [[link(2, 'cache'), link(1, 'main')], [link(2, 'cache')]]
    )
    expect(merged.map((db) => db.name)).toEqual(['cache', 'main'])
    expect(merged[0].usedBy.map((m) => m.name)).toEqual(['web', 'api'])
    expect(merged[1].usedBy.map((m) => m.name)).toEqual(['web'])
  })

  test('a failed or pending service contributes nothing', () => {
    expect(mergeGroupDatabases([web, api], [undefined, undefined])).toEqual([])
  })

  test('domains carry their service and sort by hostname', () => {
    const domain = (id: number, name: string) =>
      ({ id, domain: name, status: 'active' }) as CustomDomainResponse
    const merged = mergeGroupDomains(
      [web, api],
      [[domain(1, 'shop.example.test')], [domain(2, 'api.example.test')]]
    )
    expect(merged.map((d) => [d.domain, d.service.slug])).toEqual([
      ['api.example.test', 'api'],
      ['shop.example.test', 'web'],
    ])
  })

  test('deployments are newest first across services', () => {
    const deployment = (id: number, created_at: number) =>
      ({ id, created_at }) as DeploymentResponse
    const merged = mergeGroupDeployments(
      [web, api],
      [[deployment(1, 100), deployment(2, 300)], [deployment(3, 200)]]
    )
    expect(merged.map((d) => [d.id, d.service.slug])).toEqual([
      [2, 'web'],
      [3, 'api'],
      [1, 'web'],
    ])
  })

  test('backups are newest first across databases', () => {
    const backup = (id: number, started_at: string) =>
      ({ id, started_at }) as ServiceBackupEntryResponse
    const merged = mergeGroupBackups(
      [
        { id: 7, name: 'main', type: 'postgres' },
        { id: 8, name: 'cache', type: 'redis' },
      ],
      [[backup(1, '2026-01-01T00:00:00Z')], [backup(2, '2026-01-02T00:00:00Z')]]
    )
    expect(merged.map((b) => [b.id, b.database.name])).toEqual([
      [2, 'cache'],
      [1, 'main'],
    ])
  })
})
