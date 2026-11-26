// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SourceBackupEntry } from '@/api/client/types.gen'
import { restoreSourceBackupsQuery } from '@/pages/service-restore/restore-queries'
import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import { CreationBackupSource } from './CreationBackupSource'

afterEach(cleanup)

const SOURCES = {
  isPending: false,
  isError: false,
  data: [{ id: 7, name: 'Offsite bucket', is_default: true }],
  refetch: () => {},
}

function backup(overrides: Partial<SourceBackupEntry>): SourceBackupEntry {
  return {
    backup_id: '',
    backup_type: 'full',
    created_at: '2026-01-01T00:00:00Z',
    engine: 'postgres',
    format: 'walg',
    id: 0,
    location:
      's3://bucket/walg/basebackups_005/base_1_backup_stop_sentinel.json',
    metadata_location: '',
    name: 'postgres backup',
    origin_service_name: 'app-db',
    source: 's3_scan',
    state: '',
    ...overrides,
  }
}

function withBackups(backups: SourceBackupEntry[], children: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  client.setQueryData(restoreSourceBackupsQuery(7, true).queryKey, {
    backups,
    scan_errors: [],
  } as never)
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>{children}</MemoryRouter>
    </QueryClientProvider>
  )
}

test('lists only the backups this engine can start from, and picks one', () => {
  const picked: SourceBackupEntry[] = []
  const found = backup({})
  const view = render(
    withBackups(
      [
        found,
        backup({ engine: 'mariadb', origin_service_name: 'shop-db' }),
        backup({
          id: 3,
          source: 'db',
          state: 'failed',
          origin_service_name: 'broken',
        }),
      ],
      <CreationBackupSource
        engine="postgres"
        engineName="PostgreSQL"
        sources={SOURCES}
        sourceId={7}
        onSourceChange={() => {}}
        selected={null}
        onSelect={(entry) => picked.push(entry)}
      />
    )
  )
  const options = view.getAllByRole('radio')
  expect(options).toHaveLength(1)
  expect(view.getByText('Found in S3')).toBeTruthy()
  expect(view.queryByText('shop-db')).toBeNull()
  expect(view.queryByText('broken')).toBeNull()
  fireEvent.click(options[0])
  expect(picked).toEqual([found])
})

test('says when the source holds no backup of this engine', () => {
  const view = render(
    withBackups(
      [backup({ engine: 'redis' })],
      <CreationBackupSource
        engine="postgres"
        engineName="PostgreSQL"
        sources={SOURCES}
        sourceId={7}
        onSourceChange={() => {}}
        selected={null}
        onSelect={() => {}}
      />
    )
  )
  expect(view.getByText('No PostgreSQL backups in this source')).toBeTruthy()
})

test('without a storage source it says what is missing and links to add one', () => {
  const view = render(
    withBackups(
      [],
      <CreationBackupSource
        engine="postgres"
        engineName="PostgreSQL"
        sources={{ ...SOURCES, data: [] }}
        sourceId={undefined}
        onSourceChange={() => {}}
        selected={null}
        onSelect={() => {}}
      />
    )
  )
  expect(view.getByText('No storage source is configured')).toBeTruthy()
  expect(
    view
      .getByRole('link', { name: 'Add a storage source' })
      .getAttribute('href')
  ).toBe('/backups/s3-sources/new')
})
