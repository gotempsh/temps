// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { AccessBundleEditor } from './AccessBundleEditor'
installBrowserWindow()
afterEach(cleanup)
test('bundle editor without management capability names the instance permission', () => {
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <AccessBundleEditor
        vaultId="vault_fixture"
        services={[]}
        canWrite={false}
        onClose={() => {}}
        initialBundle={{
          id: 'bundle_fixture',
          vault_id: 'vault_fixture',
          name: 'Fixture',
          service_ids: ['svc_fixture'],
          ttl_seconds: 900,
          version: 1,
          created_at: '2026-01-01T00:00:00Z',
          updated_at: '2026-01-01T00:00:00Z',
        }}
      />
    </QueryClientProvider>
  )
  expect(
    view.getByText('Instance administrator permission is required')
  ).toBeTruthy()
  client.clear()
})
