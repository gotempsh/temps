// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { listBackupAlertsOptions } from '@/lib/backup-alerts'
import { BackupAlertsButton } from './BackupAlertsButton'

function signedInAs(role: string): UserResponse {
  return {
    id: 3,
    name: 'Example User',
    username: 'example',
    avatar_url: '',
    mfa_enabled: false,
    role,
  }
}

function render(client: QueryClient, user: UserResponse) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user,
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BackupAlertsButton />
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function createClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
}

test('a non-admin sees no backup alert bell and never polls the alerts', () => {
  for (const role of ['user', 'reader']) {
    const client = createClient()
    expect(render(client, signedInAs(role))).toBe('')
    // The admin-only endpoint was never queried for this role.
    const query = client
      .getQueryCache()
      .find({ queryKey: listBackupAlertsOptions().queryKey })
    expect(query?.state.fetchStatus ?? 'idle').toBe('idle')
    expect(query?.state.dataUpdatedAt ?? 0).toBe(0)
    client.clear()
  }
})

test('an instance admin sees the bell with its alert count', () => {
  for (const role of ['admin', 'platform_admin']) {
    const client = createClient()
    client.setQueryData(listBackupAlertsOptions().queryKey, { alerts: [] })
    const html = render(client, signedInAs(role))
    expect(html).toContain('Backup alerts (no alerts)')
    client.clear()
  }
})
