// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  QueryClient,
  QueryClientProvider,
  QueryObserver,
} from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import {
  backupAlertsQueryOptions,
  listBackupAlertsOptions,
} from '@/lib/backup-alerts'
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

const realFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = realFetch
})

/**
 * Subscribe an observer with the options the button uses -- the same thing
 * `useQuery` does on mount -- and count the requests that reach `fetch`.
 * Server rendering never starts a fetch, so this is what proves the poll is
 * (or is not) sent.
 */
async function requestsMadeFor(isInstanceAdmin: boolean): Promise<string[]> {
  const requested: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    requested.push(String(input))
    return new Response(JSON.stringify({ alerts: [] }), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  }) as typeof fetch
  const client = createClient()
  const observer = new QueryObserver(
    client,
    backupAlertsQueryOptions(isInstanceAdmin)
  )
  const unsubscribe = observer.subscribe(() => {})
  // Let a started fetch settle.
  await new Promise((resolve) => setTimeout(resolve, 20))
  unsubscribe()
  client.clear()
  return requested
}

test('a non-admin never requests the admin-only backup alerts', async () => {
  expect(await requestsMadeFor(false)).toEqual([])
})

test('an instance admin polls the backup alerts', async () => {
  expect(await requestsMadeFor(true)).toEqual(['/api/backups/alerts'])
})

test('a non-admin sees no backup alert bell', () => {
  for (const role of ['user', 'reader']) {
    const client = createClient()
    expect(render(client, signedInAs(role))).toBe('')
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
