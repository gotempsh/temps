// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { AuthContext } from '@/contexts/AuthContext-shared'
import {
  kvStatusOptions,
  blobStatusOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { PlatformServices } from './PlatformServices'

const keys = [kvStatusOptions().queryKey, blobStatusOptions().queryKey]
const status = {
  enabled: true,
  healthy: true,
  docker_image: 'test:1',
  version: '1',
}
function clientWithStatus() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  for (const queryKey of keys) client.setQueryData(queryKey, status)
  return client
}
function fail(
  client: QueryClient,
  index: number,
  error: unknown,
  cached = false
) {
  const query = client.getQueryCache().find({ queryKey: keys[index] })!
  query.setState({
    status: 'error',
    error: error as Error,
    fetchStatus: 'idle',
    ...(!cached ? { data: undefined, dataUpdatedAt: 0 } : {}),
  })
}
function render(client: QueryClient, role = 'admin') {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: {
            id: 1,
            name: 'Operator',
            username: 'operator',
            role,
            avatar_url: '',
            mfa_enabled: false,
          },
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <PlatformServices />
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

for (const error of [
  { title: 'Forbidden', status: 403 },
  { title: 'Server error', status: 500 },
  new TypeError('Failed to fetch'),
]) {
  test(`one failed status read preserves the other service: ${String(error)}`, () => {
    const client = clientWithStatus()
    fail(client, 0, error)
    const html = render(client)
    expect(html).toContain(
      error instanceof TypeError || error.status !== 403
        ? 'KV Store status unavailable'
        : 'KV Store status: access denied'
    )
    expect(html).toContain('Status unknown')
    expect(html).not.toContain('Enable KV Store')
    expect(html).not.toContain('Disabled')
    expect(html).toContain('Disable Blob Storage')
    expect(html).toContain('Retry')
    client.clear()
  })
}
for (const error of [
  { title: 'Forbidden', detail: 'Denied' },
  { title: 'Server error', status: 500 },
  new TypeError('Failed to fetch'),
]) {
  test(`both failures show unknown with recovery: ${String(error)}`, () => {
    const client = clientWithStatus()
    for (const i of [0, 1]) fail(client, i, error)
    const html = render(client)
    expect(html.match(/Status unknown/g)).toHaveLength(2)
    expect(html).toContain('Retry')
    expect(html).not.toContain('Enable KV Store')
    expect(html).not.toContain('Enable Blob Storage')
    client.clear()
  })
}

test('cached refresh failure preserves stale state and blocks changes; successful retry restores actions', async () => {
  const client = clientWithStatus()
  fail(client, 0, new TypeError('Failed to fetch'), true)
  const html = render(client)
  expect(html).toContain('Status unavailable · stale')
  expect(html).toContain('Last known state: Healthy')
  expect(html).toContain('Last checked:')
  expect(actionButton(html)).toContain('disabled=""')
  await client.fetchQuery({
    queryKey: keys[0],
    queryFn: async () => status,
    staleTime: 0,
  })
  const recovered = render(client)
  expect(recovered).not.toContain('status unavailable')
  expect(actionButton(recovered)).not.toContain('disabled=""')
  client.clear()
})
test('non-admin users cannot change a successfully read service', () => {
  const client = clientWithStatus()
  const html = render(client, 'viewer')
  expect(html).toContain('Administrator permission is required')
  expect(actionButton(html)).toContain('disabled=""')
  client.clear()
})

function actionButton(html: string) {
  return (
    Array.from(
      html.matchAll(/<button\b[^>]*>[\s\S]*?<\/button>/g),
      (match) => match[0]
    ).find((button) => button.includes('Disable KV Store')) ?? ''
  )
}
