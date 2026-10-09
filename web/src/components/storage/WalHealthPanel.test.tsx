// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { getExternalServiceBackupCapabilityQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  ExternalServiceBackupCapabilityResponse,
  PostgresWalHealth,
} from '@/api/client/types.gen'
import { WalHealthPanel } from './WalHealthPanel'

function renderPanel(settings: Partial<PostgresWalHealth>) {
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
