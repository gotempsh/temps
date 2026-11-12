// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { restoreOwnBackupsQuery, restoreSourcesQuery } from './restore-queries'

installBrowserWindow()
afterEach(() => undefined)

async function observeSources(status: number) {
  const api = mockApi({
    'GET /backups/s3-sources': () =>
      jsonResponse({ title: 'Problem', status, detail: 'injected' }, status),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const observer = new QueryObserver(client, {
    ...restoreSourcesQuery(),
    retry: false,
  })
  const unsubscribe = observer.subscribe(() => undefined)
  await observer.refetch()
  return { client, api, unsubscribe }
}

test('a refused source list is not reported by the shell, the page falls back to its own backups', async () => {
  const { client, api, unsubscribe } = await observeSources(403)
  expect(client.getQueryCache().findAll()[0]?.state.status).toBe('error')
  expect(collectRouteQueryFailures(client)).toEqual([])
  unsubscribe()
  api.restore()
})

test('a real failure of the source list is still reported', async () => {
  const { client, api, unsubscribe } = await observeSources(500)
  expect(client.getQueryCache().findAll()[0]?.state.status).toBe('error')
  expect(collectRouteQueryFailures(client)).toHaveLength(1)
  unsubscribe()
  api.restore()
})

const ownBackup = (id: number) => ({
  backup_id: `00000000-0000-4000-8000-${String(id).padStart(12, '0')}`,
  backup_type: 'full',
  compression_type: 'gzip',
  external_service_backup_id: id,
  id,
  name: `postgres backup ${id}`,
  s3_location: `backups/synthetic-db/${id}`,
  s3_source_id: 2,
  s3_source_name: 'primary-bucket',
  started_at: '2026-01-01T10:00:00Z',
  state: 'completed',
})

test("an older recovery point stays reachable: every page of the database's own backups is read", async () => {
  const pages: Record<string, number[]> = {
    '1': Array.from({ length: 100 }, (_, i) => 130 - i),
    '2': Array.from({ length: 30 }, (_, i) => 30 - i),
  }
  const requested: string[] = []
  const api = mockApi({
    'GET /backups/external-services/7/backups': (request) => {
      const page = new URL(request.url).searchParams.get('page') ?? '1'
      requested.push(page)
      return jsonResponse({
        page: Number(page),
        page_size: 100,
        total: 130,
        backups: (pages[page] ?? []).map(ownBackup),
      })
    },
  })
  const client = new QueryClient()
  const merged = await client.fetchQuery(restoreOwnBackupsQuery(7))
  expect(requested).toEqual(['1', '2'])
  expect(merged.backups).toHaveLength(130)
  expect(merged.total).toBe(130)
  // The oldest backup, which a first page alone would have dropped.
  expect(merged.backups.some((backup) => backup.id === 1)).toBe(true)
  api.restore()
})

test('a short list is read in a single request', async () => {
  const requested: string[] = []
  const api = mockApi({
    'GET /backups/external-services/7/backups': (request) => {
      requested.push(new URL(request.url).searchParams.get('page') ?? '1')
      return jsonResponse({
        page: 1,
        page_size: 100,
        total: 2,
        backups: [ownBackup(2), ownBackup(1)],
      })
    },
  })
  const merged = await new QueryClient().fetchQuery(restoreOwnBackupsQuery(7))
  expect(requested).toEqual(['1'])
  expect(merged.backups).toHaveLength(2)
  api.restore()
})
