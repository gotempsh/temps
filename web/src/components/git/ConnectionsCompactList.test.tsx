// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type {
  ConnectionResponse,
  UserResponse,
  ProviderResponse,
} from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { ConnectionsCompactList } from './ConnectionsCompactList'

installBrowserWindow()
afterEach(cleanup)

function list(
  authMethod: string,
  expired = false,
  unhealthy = false,
  variant: 'single-line' | 'two-line' | 'avatar' = 'single-line',
  owner: number | null = 4,
  providerType = 'github',
  healthMessage: string | null = null
) {
  const connection = {
    id: 11,
    provider_id: 7,
    user_id: owner,
    account_name: 'example-account',
    account_type: 'User',
    installation_id: authMethod === 'github_app' ? '123' : null,
    has_authenticated_credentials: true,
    is_active: true,
    is_expired: expired,
    syncing: false,
    synced_repository_count: 0,
    last_synced_at: null,
    health_status: unhealthy ? 'unhealthy' : 'healthy',
    health_message: healthMessage,
    last_health_check_at: null,
    consecutive_health_failures: 0,
    last_sync_error: null,
    last_sync_error_at: null,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  } as ConnectionResponse
  const provider = {
    id: 7,
    provider_type: providerType,
    auth_method: authMethod,
  } as ProviderResponse
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <AuthContext.Provider
        value={{
          user: { id: 4 } as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter>
          <ConnectionsCompactList
            connections={[connection]}
            provider={provider}
            onSyncRepository={() => {}}
            isSyncing={false}
            variant={variant}
          />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

for (const method of ['oauth', 'pat', 'github_app']) {
  test(`Reauthorize menu visibility for ${method}`, async () => {
    const view = list(method)
    await act(async () => {
      fireEvent.pointerDown(view.getByRole('button', { name: 'Open menu' }), {
        button: 0,
        ctrlKey: false,
      })
    })
    expect(view.queryByRole('menuitem', { name: 'Reauthorize' }) !== null).toBe(
      method === 'oauth'
    )
  })
}

for (const variant of ['single-line', 'two-line', 'avatar'] as const) {
  for (const [expired, unhealthy] of [
    [true, false],
    [false, true],
  ]) {
    test(`${variant} exposes direct recovery when expired=${expired}, unhealthy=${unhealthy}`, () => {
      const view = list('oauth', expired, unhealthy, variant)
      expect(view.getByRole('button', { name: 'Reauthorize' })).toBeTruthy()
    })
  }
}

test('unhealthy PAT and App connections have no OAuth recovery button', () => {
  for (const method of ['pat', 'github_app']) {
    const view = list(method, true, true)
    expect(view.queryByRole('button', { name: 'Reauthorize' })).toBeNull()
    cleanup()
  }
})

for (const owner of [null, 9]) {
  test(`OAuth recovery is hidden for connection owner ${owner}`, async () => {
    const view = list('oauth', true, true, 'single-line', owner)
    expect(view.queryByRole('button', { name: 'Reauthorize' })).toBeNull()
    await act(async () => {
      fireEvent.pointerDown(view.getByRole('button', { name: 'Open menu' }), {
        button: 0,
        ctrlKey: false,
      })
    })
    expect(view.queryByRole('menuitem', { name: 'Reauthorize' })).toBeNull()
  })
}

const REJECTED = "GitLab no longer accepts this connection's authorization"
const RAW_RESPONSE =
  'Failed to refresh token: 400 Bad Request - {"error":"invalid_grant","error_description":"raw provider response"}'

for (const variant of ['single-line', 'two-line', 'avatar'] as const) {
  test(`a rejected GitLab authorization replaces the raw response beside Reauthorize (${variant})`, () => {
    const view = list('oauth', false, true, variant, 4, 'gitlab', RAW_RESPONSE)
    const message = view.getByText(REJECTED)
    const reauthorize = view.getByRole('button', { name: 'Reauthorize' })
    expect(
      message.parentElement?.contains(reauthorize),
      'the message must sit next to the action'
    ).toBe(true)
    expect(view.queryByText(/raw provider response/)).toBeNull()
  })

  test(`a blocked GitLab connection with stale healthy status still says so (${variant})`, () => {
    const view = list('oauth', true, false, variant, 4, 'gitlab')
    expect(view.getByText(REJECTED)).toBeTruthy()
    expect(view.getByRole('button', { name: 'Reauthorize' })).toBeTruthy()
  })
}

test('another unhealthy reason keeps its own message and no rejection copy', () => {
  const view = list(
    'oauth',
    false,
    true,
    'single-line',
    4,
    'gitlab',
    'Provider error: HTTP 503'
  )
  expect(view.queryByText(REJECTED)).toBeNull()
  expect(view.getByText('Provider error: HTTP 503')).toBeTruthy()
})

test('a user who cannot reauthorize still sees why the connection stopped', () => {
  const view = list('oauth', true, false, 'single-line', 99, 'gitlab')
  expect(view.getByText(REJECTED)).toBeTruthy()
  expect(view.queryByRole('button', { name: 'Reauthorize' })).toBeNull()
})
