// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { getExternalServiceBackupCapabilityQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  ExternalServiceBackupCapabilityResponse,
  PostgresWalHealth,
} from '@/api/client/types.gen'
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { WalHealthPanel } from './WalHealthPanel'

installBrowserWindow()

function renderPanel(settings: Partial<PostgresWalHealth>, isImported = false) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  const snapshot: PostgresWalHealth = {
    probed_at: '2026-01-01T00:00:00Z',
    archive_mode: 'on',
    archive_command: '/bin/true',
    archive_backlog: 0,
    pg_wal_bytes: 1024,
    max_wal_size_bytes: 1024 * 1024,
    oldest_wal_age_secs: 0,
    stale_slots: [],
    warnings: [{ kind: 'archive_mode_without_command' }],
    ...settings,
  }
  client.setQueryData(['wal-health', 7], {
    wal_health: snapshot,
  })
  const capability: ExternalServiceBackupCapabilityResponse = {
    cloud_backup_compatible: true,
    artifact: 'walg_repository',
    engine: 'postgres_walg',
    verified: true,
    wal_g_installed: true,
  }
  client.setQueryData(
    getExternalServiceBackupCapabilityQueryKey({ path: { id: 7 } }),
    capability
  )
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <WalHealthPanel
        serviceId={7}
        serviceType="postgres"
        onBackup={() => {}}
        isImported={isImported}
      />
    </QueryClientProvider>
  )
}

describe('post-restore WAL archiving guidance', () => {
  test('an intentional disabled policy stays visible with a full-backup action', () => {
    const markup = renderPanel({ archive_mode: 'off', warnings: [] })
    expect(markup).toContain('Continuous WAL archiving is disabled')
    expect(markup).toContain('protect its source backup')
    expect(markup).toContain('Restarting alone keeps archiving disabled')
    expect(markup).toContain('Create full backup')
  })

  test('an imported disabled policy requires external startup changes instead of a failed repair', () => {
    const markup = renderPanel({ archive_mode: 'off', warnings: [] }, true)
    expect(markup).toContain('Continuous WAL archiving is disabled')
    expect(markup).toContain('startup settings are managed outside Temps')
    expect(markup).toContain('Enable archive_mode')
    expect(markup).toContain('restart it before creating a full backup')
    expect(markup).toContain('restore into a new managed service')
    expect(markup).not.toContain('Create full backup')
    expect(markup).not.toContain('Restarting alone keeps archiving disabled')
  })

  test('a no-op command explains discarded WAL instead of disk backlog', () => {
    const markup = renderPanel({ archive_command: '/bin/true' })
    expect(markup).toContain('archive_command discards WAL')
    expect(markup).toContain('reports success without storing WAL')
    expect(markup).not.toContain('archive_command is empty')
    expect(markup).not.toContain('fill disk')
    expect(markup).not.toContain('Stop')
  })

  test('an empty command explains pending WAL and disk risk', () => {
    const markup = renderPanel({ archive_command: '' })
    expect(markup).toContain('archive_command is empty')
    expect(markup).toContain('can fill disk')
    expect(markup).not.toContain('discards WAL')
  })
})

describe('backup compatibility check that cannot be answered', () => {
  function renderUnanswered(status: number) {
    const api = mockApi({
      'GET /backups/external-services/7/capability': () =>
        jsonResponse(
          status === 403
            ? {
                type: 'about:blank',
                title: 'Forbidden',
                status,
                detail:
                  'You do not have access to the external service (7) involved in this backup',
              }
            : { title: 'Internal Server Error', status, detail: 'boom' },
          status
        ),
    })
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    const view = render(
      <QueryClientProvider client={client}>
        <WalHealthPanel serviceId={7} serviceType="postgres" />
      </QueryClientProvider>
    )
    return { api, client, view }
  }

  afterEach(cleanup)

  test('a refusal shows nothing: no error alert, no internal id, no retry', async () => {
    const { api, client, view } = renderUnanswered(403)
    await waitFor(() =>
      expect(
        client
          .getQueryCache()
          .findAll()
          .some((q) => q.state.status === 'error')
      ).toBe(true)
    )
    expect(view.container.textContent).not.toContain(
      'Backup compatibility unavailable'
    )
    expect(view.container.textContent).not.toContain('external service (7)')
    expect(view.queryByRole('button', { name: 'Check again' })).toBeNull()
    expect(collectRouteQueryFailures(client)).toEqual([])
    api.restore()
  })

  test('a 403 whose body names no status or title is still treated as a refusal', async () => {
    const api = mockApi({
      'GET /backups/external-services/7/capability': () =>
        new Response('Forbidden', { status: 403 }),
    })
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    const view = render(
      <QueryClientProvider client={client}>
        <WalHealthPanel serviceId={7} serviceType="postgres" />
      </QueryClientProvider>
    )
    await waitFor(() =>
      expect(
        client
          .getQueryCache()
          .findAll()
          .some((q) => q.state.status === 'error')
      ).toBe(true)
    )
    expect(view.container.textContent).not.toContain(
      'Backup compatibility unavailable'
    )
    api.restore()
  })

  test('a real failure of the check is still reported, with a retry', async () => {
    const { api, client, view } = renderUnanswered(500)
    expect(
      await view.findByText('Backup compatibility unavailable')
    ).toBeTruthy()
    expect(view.getByRole('button', { name: 'Check again' })).toBeTruthy()
    // The panel explains it itself; the shell does not repeat it.
    expect(collectRouteQueryFailures(client)).toEqual([])
    api.restore()
  })
})
