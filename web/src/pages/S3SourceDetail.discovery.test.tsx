// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, waitFor } from '@testing-library/react'
import { Link, MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { createAppQueryClient } from '@/test/app-query-client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { restoreSourceBackupsQuery } from './service-restore/restore-queries'
import { S3SourceDetail } from './S3SourceDetail'

installBrowserWindow()

test('discovery stays with its source and live tracked refreshes never rescan the bucket', async () => {
  const created_at = '2026-01-01T12:00:00Z'
  const source = (id: number) => ({
    id,
    name: `source-${id}`,
    bucket_name: 'backups-bucket',
    region: 'us-east-1',
    created_at,
  })
  const row = {
    id: 0,
    backup_id: '',
    name: 'snapshot-a',
    origin_service_name: 'service-a',
    location: 'snapshot-a',
    engine: 'postgres',
    format: 'walg',
    source: 's3_scan',
    state: 'completed',
    created_at,
  }
  const tracked = {
    ...row,
    id: 1,
    backup_id: 'backup-b',
    location: 'snapshot-b',
    origin_service_name: 'service-b',
    source: 'db',
  }
  let finish: ((response: Response) => void) | undefined
  let scans = 0
  let publishTracked = false
  const index = (backups: (typeof row)[], scan_errors: string[] = []) =>
    jsonResponse({ backups, last_updated: created_at, scan_errors })
  const api = mockApi({
    'GET /backups/schedules': () => jsonResponse([]),
    'GET /backups/s3-sources/1': () => jsonResponse(source(1)),
    'GET /backups/s3-sources/2': () => jsonResponse(source(2)),
    'GET /backups/s3-sources/1/backups': (req) => {
      if (new URL(req.url).searchParams.get('include_s3_scan') !== 'true')
        return index(publishTracked ? [tracked] : [])
      scans++
      if (scans === 1)
        return new Promise<Response>((resolve) => {
          finish = resolve
        })
      return index([], ['Could not list backups'])
    },
    'GET /backups/s3-sources/2/backups': (req) => {
      expect(new URL(req.url).searchParams.get('include_s3_scan')).toBe('false')
      return index([])
    },
  })
  const client = createAppQueryClient()
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/sources/1']}>
        <Link to="/sources/1">View first source</Link>
        <Link to="/sources/2">View second source</Link>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/sources/:id" element={<S3SourceDetail />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  try {
    const scanButton = () =>
      view.getByRole('button', { name: /Search S3 for backups/ })
    await view.findByText('source-1')
    fireEvent.click(scanButton())
    await waitFor(() => expect(finish).toBeDefined())
    fireEvent.click(view.getByRole('link', { name: 'View second source' }))
    await view.findByText('source-2')
    finish?.(index([row]))
    await waitFor(() => expect(client.isFetching()).toBe(0))
    expect(view.queryByText('service-a')).toBeNull()
    fireEvent.click(view.getByRole('link', { name: 'View first source' }))
    await view.findByText('service-a')
    // One name for a backup this server does not track, as in the restore
    // picker; never "orphan".
    expect(view.getByText('Found in S3')).toBeTruthy()
    expect(view.queryByText('orphan')).toBeNull()
    publishTracked = true
    await client.invalidateQueries({
      queryKey: restoreSourceBackupsQuery(1, false).queryKey,
      exact: true,
    })
    await view.findByText('service-b')
    expect(view.getByText('service-a')).toBeTruthy()
    expect(scans).toBe(1)
    fireEvent.click(scanButton())
    await view.findByText('Could not list backups')
    expect(view.getByText('service-a')).toBeTruthy()
    expect(view.getByText('From an earlier scan')).toBeTruthy()
  } finally {
    view.unmount()
    client.clear()
    api.restore()
  }
})
