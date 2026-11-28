// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import {
  getBackupScheduleOptions,
  listScheduleServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  BackupScheduleResponse,
  ExternalServiceSummary,
} from '@/api/client/types.gen'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { listScheduleRunsOptions } from '@/lib/schedule-runs'
import { ScheduleDetail } from './ScheduleDetail'

function schedule(
  overrides: Partial<BackupScheduleResponse>
): BackupScheduleResponse {
  return {
    id: 12,
    name: 'Control plane hourly',
    backup_type: 'full',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    enabled: true,
    retention_period: 7,
    s3_source_id: 4,
    schedule_expression: '0 0 * * * *',
    tags: [],
    target_all_services: false,
    include_control_plane: true,
    ...overrides,
  }
}

function render(
  value: BackupScheduleResponse,
  attached: Array<Pick<ExternalServiceSummary, 'id' | 'name' | 'service_type'>>
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(
    getBackupScheduleOptions({ path: { id: 12 } }).queryKey,
    value
  )
  client.setQueryData(
    listScheduleServicesOptions({ path: { id: 12 } }).queryKey,
    attached as ExternalServiceSummary[]
  )
  client.setQueryData(listScheduleRunsOptions(12, 1, 20).queryKey, {
    runs: [],
    total: 0,
    page: 1,
    page_size: 20,
  })
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/backups/schedules/12']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/backups/schedules/:id" element={<ScheduleDetail />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return html
}

test('a control-plane-only schedule says so instead of showing no services', () => {
  const html = render(schedule({}), [])

  expect(html).toContain('Only the Temps control plane')
  expect(html).toContain('Control plane only')
  expect(html).not.toContain('No services attached yet')
  expect(html).not.toContain('Nothing to back up')
})

test('a schedule without databases or the control plane is flagged as backing up nothing', () => {
  const html = render(schedule({ include_control_plane: false }), [])

  expect(html).toContain('Nothing to back up')
  expect(html).toContain('Nothing: no databases are attached')
})

test('databases plus the control plane are counted', () => {
  const html = render(schedule({}), [
    { id: 7, name: 'app-db', service_type: 'postgres' },
  ])

  expect(html).toContain('1 database and the Temps control plane')
  expect(html).toContain('app-db')
})
