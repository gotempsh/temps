// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { listS3SourcesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { S3SourceResponse } from '@/api/client/types.gen'
import { S3SourcesManagement } from './S3SourcesManagement'

const listKey = listS3SourcesOptions().queryKey

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'Failed to list S3 sources: decryption key unavailable',
}
const forbidden = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires BackupsRead permission',
}

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  client
    .getQueryCache()
    .build(client, { queryKey })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}

function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/backups']}>
        <S3SourcesManagement />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function expectNoEmptyState(html: string) {
  expect(html).not.toContain('No S3 sources configured')
  expect(html).not.toContain('Add an S3 source to store your backups')
  expect(html).not.toContain('Add S3 Source')
}

for (const error of [serverError, new TypeError('Failed to fetch')]) {
  test(`failed S3 source read is not an empty list: ${String(error)}`, () => {
    const client = createClient()
    fail(client, listKey, error)
    const html = render(client)
    expect(html).toContain('S3 sources unavailable')
    expect(html).toContain('Retry')
    expectNoEmptyState(html)
    if (!(error instanceof TypeError)) {
      expect(html).toContain(serverError.detail)
    }
    client.clear()
  })
}

test('forbidden S3 source read says access denied, not "none configured"', () => {
  const client = createClient()
  fail(client, listKey, forbidden)
  const html = render(client)
  expect(html).toContain('S3 sources: access denied')
  expect(html).toContain(forbidden.detail)
  expectNoEmptyState(html)
  client.clear()
})

test('verified empty S3 source list keeps the Add S3 Source CTA', () => {
  const client = createClient()
  client.setQueryData(listKey, [] as S3SourceResponse[])
  const html = render(client)
  expect(html).toContain('No S3 sources configured')
  expect(html).toContain('Add S3 Source')
  expect(html).toContain('href="/backups/s3-sources/new"')
  expect(html).not.toContain('unavailable')
  expect(html).not.toContain('access denied')
  client.clear()
})

test('cached S3 sources stay visible when a refresh fails', () => {
  const client = createClient()
  const cached: S3SourceResponse[] = [
    {
      id: 3,
      name: 'Primary backups',
      bucket_name: 'example-backups',
      bucket_path: '/',
      region: 'us-east-1',
      access_key_id: '***',
      is_default: true,
      managed_by_cloud: false,
      created_at: Date.parse('2026-01-01T00:00:00Z'),
      updated_at: Date.parse('2026-01-01T00:00:00Z'),
    },
  ]
  client.setQueryData(listKey, cached)
  fail(client, listKey, serverError)
  const html = render(client)
  expect(html).toContain('Primary backups')
  expect(html).toContain('S3 sources unavailable')
  expect(html).toContain('Showing last-known data')
  expect(html).not.toContain('No S3 sources configured')
  client.clear()
})
