// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import type { RestoreRunView } from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { ServiceRestore } from './ServiceRestore'
import {
  restoreCapabilitiesQuery,
  restoreRunQuery,
  restoreServiceQuery,
  restoreSourcesQuery,
  serviceRestoreRunsQuery,
} from './service-restore/restore-queries'
import { toQueryError } from './service-restore/restore-state'

const SERVICE_ID = 7
const T0 = Date.UTC(2026, 0, 2, 3, 4, 5)

type Seed =
  { data: unknown; updatedAt?: number } | { error: unknown } | 'pending'

interface PageSeeds {
  url?: string
  locationState?: unknown
  service?: Seed
  capabilities?: Seed
  sources?: Seed
  runs?: Seed
  run?: {
    id: number
    seed: Seed | { data: unknown; updatedAt?: number; error: unknown }
  }
}

const serviceData = {
  service: { id: SERVICE_ID, name: 'orders-db', service_type: 'postgres' },
  sensitive_parameters: [],
}
const capabilitiesData = {
  service_type: 'postgres',
  capabilities: {},
  restore_in_place: true,
  restore_to_new_service: true,
  pitr: false,
  suggested_new_service_name: 'orders-db-restored',
}
const sourceData = [
  {
    id: 1,
    name: 'primary-bucket',
    bucket_name: 'backups',
    is_default: true,
  },
]

function runRow(overrides: Partial<RestoreRunView> = {}): RestoreRunView {
  return {
    id: 31,
    created_at: '2026-01-02T03:00:00Z',
    mode: 'in_place',
    phase: 'restore',
    source_backup_id: 4,
    source_backup: { id: 4 },
    source_service_id: SERVICE_ID,
    status: 'running',
    ...overrides,
  }
}

const problem = (status: number | undefined) =>
  status === undefined
    ? toQueryError(new TypeError('Failed to fetch'), undefined)
    : toQueryError({ title: 'Problem', detail: 'injected' }, status)

function seed(
  client: QueryClient,
  queryKey: readonly unknown[],
  value:
    Seed | { data: unknown; updatedAt?: number; error: unknown } | undefined
) {
  if (value === undefined || value === 'pending') return
  if ('data' in value) {
    client.setQueryData(queryKey, value.data, { updatedAt: value.updatedAt })
  }
  if ('error' in value) {
    const query = client.getQueryCache().build(client, { queryKey })
    query.setState({
      status: 'error',
      error: value.error as Error,
      fetchStatus: 'idle',
    })
  }
}

function urlParts(url: string) {
  const [pathname, search = ''] = url.split('?')
  return { pathname, search: search ? `?${search}` : '' }
}

const START_DISABLED =
  /<button[^>]*disabled=""[^>]*>(?:(?!<\/button>).)*Start restore/

function renderPage(seeds: PageSeeds = {}) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  seed(
    client,
    restoreServiceQuery(SERVICE_ID).queryKey,
    seeds.service ?? { data: serviceData }
  )
  seed(
    client,
    restoreCapabilitiesQuery(SERVICE_ID).queryKey,
    seeds.capabilities ?? { data: capabilitiesData }
  )
  seed(
    client,
    restoreSourcesQuery().queryKey,
    seeds.sources ?? { data: sourceData }
  )
  seed(
    client,
    serviceRestoreRunsQuery(SERVICE_ID).queryKey,
    seeds.runs ?? { data: [] }
  )
  if (seeds.run) {
    seed(client, restoreRunQuery(seeds.run.id).queryKey, seeds.run.seed)
  }
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter
        initialEntries={[
          {
            ...urlParts(seeds.url ?? `/storage/${SERVICE_ID}/restore`),
            state: seeds.locationState,
          },
        ]}
      >
        <BreadcrumbProvider>
          <Routes>
            <Route path="/storage/:id/restore" element={<ServiceRestore />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return markup
}

describe('service load errors (#1238)', () => {
  test('loading shows a skeleton, not a spinner', () => {
    const markup = renderPage({ service: 'pending' })
    expect(markup).toContain('Loading restore options')
    expect(markup).toContain('animate-pulse')
    expect(markup).not.toContain('animate-spin')
  })

  test('403 explains who can grant access and offers Retry and Back', () => {
    const markup = renderPage({ service: { error: problem(403) } })
    expect(markup).toContain(
      'Ask an administrator or the project owner for access to this database.'
    )
    expect(markup).toContain('Retry')
    expect(markup).toContain('Back to databases')
    expect(markup).not.toContain('animate-spin')
    expect(markup).not.toContain('animate-pulse')
  })

  test('404 says the database is gone or the link is wrong', () => {
    const markup = renderPage({ service: { error: problem(404) } })
    expect(markup).toContain(
      'This database no longer exists or the link is wrong.'
    )
    expect(markup).toContain('Retry')
  })

  test('500 and network failures say it could not be loaded', () => {
    for (const status of [500, undefined]) {
      const markup = renderPage({ service: { error: problem(status) } })
      expect(markup).toContain(
        'Could not load this database. Check your connection and retry.'
      )
      expect(markup).toContain('Retry')
      expect(markup).not.toContain('animate-spin')
    }
  })
})

describe('capability and source errors (#1238)', () => {
  test('a capabilities failure disables restore with an inline reason', () => {
    const markup = renderPage({ capabilities: { error: problem(500) } })
    expect(markup).toContain('Could not load the restore options')
    expect(markup).toContain('Restore modes stay disabled until they load.')
    expect(markup).toMatch(START_DISABLED)
    expect(markup).toMatch(/<button[^>]*disabled=""[^>]*id="mode-in-place"/)
  })

  test('restore modes are disabled while capabilities load', () => {
    const markup = renderPage({ capabilities: 'pending' })
    expect(markup).toContain(
      'Checking which restore modes this database supports'
    )
    expect(markup).toMatch(/<button[^>]*disabled=""[^>]*id="mode-in-place"/)
  })

  test('a failed source list is an error with Retry, never "no sources"', () => {
    for (const status of [403, 404, 500, undefined]) {
      const markup = renderPage({ sources: { error: problem(status) } })
      expect(markup).toContain('Could not load storage sources')
      expect(markup).toContain('Retry')
      expect(markup).not.toContain('No storage sources yet')
    }
  })

  test('a genuinely empty source list onboards', () => {
    const markup = renderPage({ sources: { data: [] } })
    expect(markup).toContain('No storage sources yet')
    expect(markup).toContain('/backups/s3-sources/new')
    expect(markup).not.toContain('Could not load storage sources')
  })

  test('a failed active-run check blocks starting with Retry', () => {
    const markup = renderPage({ runs: { error: problem(500) } })
    expect(markup).toContain('Could not check for a running restore')
    expect(markup).toMatch(START_DISABLED)
  })
})

describe('run tracking (#1237)', () => {
  const url = `/storage/${SERVICE_ID}/restore?run=31`

  test('initial polling failure: status unknown, nothing confirmed, no Start', () => {
    const markup = renderPage({
      url,
      run: { id: 31, seed: { error: problem(500) } },
    })
    expect(markup).toContain(
      'Restore status could not be refreshed; the restore may still be running. No status has been confirmed yet.'
    )
    expect(markup).toContain('Copy run ID')
    expect(markup).toContain('Retry')
    expect(markup).toContain('Back to service')
    expect(markup).toContain('keeps running on')
    expect(markup).not.toContain('Starting…')
    expect(markup).not.toContain('Start restore')
    expect(markup).not.toMatch(
      /<button[^>]*disabled=""[^>]*>(?:(?!<\/button>).)*Back to service/
    )
  })

  test('failure after known progress shows the last confirmed phase', () => {
    const markup = renderPage({
      url,
      run: {
        id: 31,
        seed: { data: runRow(), updatedAt: T0, error: problem(undefined) },
      },
    })
    expect(markup).toContain(
      'Restore status could not be refreshed; the restore may still be running. Last confirmed phase: Restore data,'
    )
    expect(markup).toContain('(last confirmed)')
    expect(markup).not.toContain('Start restore')
  })

  test('permission failure explains signing in again', () => {
    const markup = renderPage({
      url,
      run: { id: 31, seed: { error: problem(403) } },
    })
    expect(markup).toContain('You may need to sign in again')
    expect(markup).toContain('Retry')
  })

  test('a missing run is not-found, not a failed restore', () => {
    const markup = renderPage({
      url,
      run: { id: 31, seed: { error: problem(404) } },
    })
    expect(markup).toContain('Restore run not found')
    expect(markup).not.toContain('Restore failed')
  })

  test('reload with the run in the URL keeps progress', () => {
    const markup = renderPage({
      url,
      run: { id: 31, seed: { data: runRow(), updatedAt: T0 } },
    })
    expect(markup).toContain('run #31')
    expect(markup).toContain('Restore data')
    expect(markup).not.toContain('Start restore')
  })

  test('reload without the run in the URL reattaches to the active run', () => {
    const markup = renderPage({
      runs: {
        data: [runRow({ id: 31 }), runRow({ id: 30, status: 'completed' })],
      },
    })
    expect(markup).toContain(
      'Following restore run #31, which was already in progress for this database.'
    )
    expect(markup).not.toContain('Start restore')
  })

  test('no active run in history shows the restore form', () => {
    const markup = renderPage({
      runs: { data: [runRow({ id: 30, status: 'completed' })] },
    })
    expect(markup).toContain('Start restore')
  })

  test('a 409 conflict attaches to the running restore and says so', () => {
    const markup = renderPage({
      url,
      locationState: { restoreAttach: 'already_active' },
      run: { id: 31, seed: { data: runRow(), updatedAt: T0 } },
    })
    expect(markup).toContain(
      'A restore was already running on this database, so a new one was not started. Following run #31 instead.'
    )
  })

  test('an interrupted run shows its own outcome and next steps', () => {
    const message =
      'This restore was interrupted when Temps restarted during restore. The database may be partially restored. Check its health and data before retrying.'
    const markup = renderPage({
      url,
      run: {
        id: 31,
        seed: {
          data: runRow({
            status: 'interrupted',
            error_message: message,
            finished_at: '2026-01-02T03:10:00Z',
          }),
        },
      },
    })
    expect(markup).toContain('Restore interrupted')
    expect(markup).toContain(message)
    expect(markup).toContain(`/storage/${SERVICE_ID}/logs`)
    expect(markup).toContain('Check database health')
    expect(markup).toContain('Start a new restore')
    expect(markup).not.toContain('Restore failed')
  })

  test('completed and failed runs render their terminal state', () => {
    const completed = renderPage({
      url,
      run: {
        id: 31,
        seed: { data: runRow({ status: 'completed', phase: 'completed' }) },
      },
    })
    expect(completed).toContain('completed')
    expect(completed).toContain('Start a new restore')
    expect(completed).not.toContain('keeps running on')

    const failed = renderPage({
      url,
      run: {
        id: 31,
        seed: {
          data: runRow({ status: 'failed', error_message: 'disk full' }),
        },
      },
    })
    expect(failed).toContain('Restore failed')
    expect(failed).toContain('disk full')
  })
})
