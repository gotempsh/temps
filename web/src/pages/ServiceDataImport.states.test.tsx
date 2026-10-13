// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import {
  getDataImportAvailabilityQueryKey,
  getServiceQueryKey,
  listRootContainersQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { DataImportAvailabilityResponse } from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { ServiceDataImport } from './ServiceDataImport'

const SERVICE_ID = 4

const availability: DataImportAvailabilityResponse = {
  service_id: SERVICE_ID,
  service_type: 'postgres',
  supported: true,
  available: true,
  default_timeout_minutes: 60,
  max_timeout_minutes: 1440,
  spec: {
    engine_label: 'PostgreSQL',
    source_schemes: ['postgres', 'postgresql'],
    source_url_example: 'postgres://user:password@db.example.com:5432/app',
    allowed_source_options: ['sslmode'],
    atomic: true,
    object_noun: 'table',
    max_target_length: 63,
  },
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

function renderImportPage(availabilitySeed: Seed) {
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
    availabilitySeed
  )
  seed(
    client,
    listRootContainersQueryKey({ path: { service_id: SERVICE_ID } }),
    { data: [{ name: 'shop_production' }, { name: 'shop_staging' }] }
  )
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[`/storage/${SERVICE_ID}/import-data`]}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/storage/:id/import-data"
              element={<ServiceDataImport />}
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

// Shared Card classes and section-title classes, so the console surface
// rules are checked on the rendered markup rather than on the source.
function cardClasses(markup: string): string[] {
  return [...markup.matchAll(/class="(rounded-lg border bg-card[^"]*)"/g)].map(
    (m) => m[1]
  )
}

function titleClass(markup: string, title: string): string | undefined {
  return markup.match(new RegExp(`class="([^"]*)"[^>]*>${title}<`))?.[1]
}

describe('import page after a failed availability refresh', () => {
  test('keeps the form and history under a stale-data warning', () => {
    const markup = renderImportPage({ data: availability, error: refreshError })
    expect(markup).toContain('import availability unavailable')
    expect(markup).toContain('Showing last-known data')
    expect(markup).toContain('Source connection string')
    expect(markup).toContain('Target database')
  })

  test('shows only the error when availability never loaded', () => {
    const markup = renderImportPage({ error: refreshError })
    expect(markup).toContain('import availability unavailable')
    expect(markup).not.toContain('Showing last-known data')
    expect(markup).not.toContain('Source connection string')
  })

  test('shows the form without a warning when availability loaded', () => {
    const markup = renderImportPage({ data: availability })
    expect(markup).not.toContain('import availability unavailable')
    expect(markup).toContain('Source connection string')
  })
})

describe('existing databases offered as targets', () => {
  test('are buttons, so they can be reached and chosen from the keyboard', () => {
    const markup = renderImportPage({ data: availability })
    for (const name of ['shop_production', 'shop_staging']) {
      expect(markup).toMatch(
        new RegExp(
          `<button[^>]*aria-label="Import into existing database ${name}"[^>]*>${name}</button>`
        )
      )
    }
  })
})

describe('console surfaces', () => {
  test('cards have no decorative shadow and section titles are text-lg', () => {
    const markup = renderImportPage({ data: availability })
    const cards = cardClasses(markup)
    expect(cards.length).toBeGreaterThan(0)
    for (const c of cards) expect(c).not.toContain('shadow-sm')
    for (const title of [
      'Copy a database into this service',
      'Import history',
    ]) {
      const cls = titleClass(markup, title)
      expect(cls).toBeDefined()
      expect(cls).toContain('text-lg')
      expect(cls).not.toContain('text-2xl')
    }
  })

  test('existing-database suggestions keep the shared button height', () => {
    const markup = renderImportPage({ data: availability })
    const suggestion = markup.match(
      /<button[^>]*class="([^"]*)"[^>]*aria-label="Import into existing database shop_production"/
    )
    expect(suggestion).not.toBeNull()
    expect(suggestion?.[1]).not.toMatch(/(^|\s)h-6(\s|$)/)
  })
})
