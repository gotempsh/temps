// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import {
  getDataImportAvailabilityQueryKey,
  getDataImportQueryKey,
  getServiceQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { DataImportRunResponse } from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { ServiceDataImportRun } from './ServiceDataImportRun'

const SERVICE_ID = 4
const RUN_ID = 31

const run: DataImportRunResponse = {
  id: RUN_ID,
  service_id: SERVICE_ID,
  service_type: 'postgres',
  target_database: 'shop_production',
  source: 'postgres://***:***@db.example.com:5432/shop',
  source_database: 'shop',
  replace_existing: false,
  atomic: true,
  status: 'succeeded',
  phase: 'finished',
  error_message: null,
  helper_output: 'pg_dump: dumped 3 tables',
  target_object_count: 3,
  target_size_bytes: 8192,
  timeout_seconds: 3600,
  created_by: 1,
  started_by: { user_id: 1, name: 'Ada', email: 'ada@example.com' },
  cancel_requested: false,
  started_at: '2026-10-08T10:00:00Z',
  finished_at: '2026-10-08T10:00:42Z',
  created_at: '2026-10-08T10:00:00Z',
  updated_at: '2026-10-08T10:00:42Z',
}

type Seed = { data?: unknown; error?: unknown }

function seed(client: QueryClient, queryKey: readonly unknown[], value: Seed) {
  if (value.data !== undefined) client.setQueryData(queryKey, value.data)
  if (value.error !== undefined) {
    const query = client.getQueryCache().build(client, { queryKey })
    query.setState({
      status: 'error',
      error: value.error as Error,
      fetchStatus: 'idle',
    })
  }
}

function renderRunPage(runSeed: Seed) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  seed(client, getServiceQueryKey({ path: { id: SERVICE_ID } }), {
    data: {
      service: { id: SERVICE_ID, name: 'orders', service_type: 'postgres' },
    },
  })
  seed(
    client,
    getDataImportAvailabilityQueryKey({ path: { id: SERVICE_ID } }),
    {
      data: {
        service_id: SERVICE_ID,
        service_type: 'postgres',
        supported: true,
        available: true,
        spec: { object_noun: 'table' },
      },
    }
  )
  seed(
    client,
    getDataImportQueryKey({ path: { id: SERVICE_ID, run_id: RUN_ID } }),
    runSeed
  )
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter
        initialEntries={[`/storage/${SERVICE_ID}/import-data/${RUN_ID}`]}
      >
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/storage/:id/import-data/:runId"
              element={<ServiceDataImportRun />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

const refreshError = new Error('Failed to fetch')

describe('run detail after a failed refresh', () => {
  test('keeps the loaded details on screen under a stale-data warning', () => {
    const markup = renderRunPage({ data: run, error: refreshError })
    expect(markup).toContain('import unavailable')
    expect(markup).toContain('Showing last-known data')
    // The details are still there.
    expect(markup).toContain('Imported 3 tables')
    expect(markup).toContain('shop_production')
    expect(markup).toContain('pg_dump: dumped 3 tables')
  })

  test('shows only the error when the run never loaded', () => {
    const markup = renderRunPage({ error: refreshError })
    expect(markup).toContain('import unavailable')
    expect(markup).not.toContain('Showing last-known data')
    expect(markup).not.toContain('Transfer output')
  })

  test('shows the details without a warning when the run loaded', () => {
    const markup = renderRunPage({ data: run })
    expect(markup).not.toContain('import unavailable')
    expect(markup).toContain('Imported 3 tables')
  })
})

describe('icon-only actions keep an accessible name on small screens', () => {
  test('a running import names the run its cancel button stops', () => {
    const markup = renderRunPage({
      data: {
        ...run,
        status: 'running',
        phase: 'transferring',
        finished_at: null,
      },
    })
    expect(markup).toContain(
      'aria-label="Cancel import 31 into shop_production"'
    )
    expect(markup).toContain('aria-label="All imports into this service"')
  })

  test('a finished import names the target of "Import again"', () => {
    const markup = renderRunPage({ data: run })
    expect(markup).toContain('aria-label="Import again into shop_production"')
  })
})
