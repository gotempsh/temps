// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import {
  environmentManager,
  focusManager,
  QueryClient,
  QueryObserver,
} from '@tanstack/react-query'
import type {
  ConnectionListResponse,
  ConnectionResponse,
  ProviderResponse,
} from '@/api/client/types.gen'
import {
  gitInstallationPollingOptions,
  hasNewGitHubInstallation,
} from './git-installation-polling'

const provider: ProviderResponse = {
  id: 7,
  name: 'Example GitHub App',
  provider_type: 'github',
  auth_method: 'github_app',
  base_url: 'https://github.com/apps/example-app',
  is_active: true,
  is_default: false,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

const installation: ConnectionResponse = {
  id: 12,
  provider_id: provider.id,
  installation_id: '456',
  account_name: 'Example account',
  account_type: 'Organization',
  consecutive_health_failures: 0,
  has_authenticated_credentials: true,
  health_status: 'healthy',
  is_active: true,
  is_expired: false,
  synced_repository_count: 0,
  syncing: false,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

async function eventually(assertion: () => void) {
  const deadline = Date.now() + 3500
  while (true) {
    try {
      assertion()
      return
    } catch (error) {
      if (Date.now() >= deadline) throw error
      await Bun.sleep(20)
    }
  }
}

beforeEach(() => {
  environmentManager.setIsServer(() => false)
  focusManager.setFocused(true)
})

afterEach(() => {
  environmentManager.setIsServer(() => true)
  focusManager.setFocused(undefined)
})

describe('GitHub installation polling', () => {
  for (const shape of ['provider', 'account-list'] as const) {
    test(`${shape} discovers an installation after the initial empty refetch`, async () => {
      const client = new QueryClient()
      let requests = 0
      let serverConnections: ConnectionResponse[] = []
      const response = (): ConnectionResponse[] | ConnectionListResponse =>
        shape === 'provider'
          ? serverConnections
          : {
              connections: serverConnections,
              page: 1,
              per_page: 20,
              total_count: serverConnections.length,
            }
      const observer = new QueryObserver(client, {
        ...gitInstallationPollingOptions([provider]),
        queryKey: ['installations', shape],
        initialData: response(),
        staleTime: Infinity,
        queryFn: async () => {
          requests += 1
          return response()
        },
      })
      const unsubscribe = observer.subscribe(() => {})
      try {
        await eventually(() => expect(requests).toBe(1))
        expect(observer.getCurrentResult().data).toEqual(response())
        serverConnections = [installation]
        await eventually(() =>
          expect(observer.getCurrentResult().data).toEqual(response())
        )
        expect(requests).toBe(2)
        unsubscribe()
        await Bun.sleep(2200)
        expect(requests).toBe(2)
      } finally {
        unsubscribe()
        client.clear()
      }
    }, 10_000)
  }

  test('pauses in an unfocused tab and refreshes fresh data immediately on return', async () => {
    const client = new QueryClient()
    let requests = 0
    const observer = new QueryObserver(client, {
      ...gitInstallationPollingOptions([provider]),
      queryKey: ['installations'],
      initialData: [] as ConnectionResponse[],
      staleTime: Infinity,
      queryFn: async () => {
        requests += 1
        return [installation]
      },
    })
    focusManager.setFocused(false)
    client.mount()
    const unsubscribe = observer.subscribe(() => {})
    try {
      await Bun.sleep(2200)
      expect(requests).toBe(0)
      focusManager.setFocused(true)
      await eventually(() =>
        expect(observer.getCurrentResult().data).toEqual([installation])
      )
      expect(requests).toBe(1)
    } finally {
      unsubscribe()
      client.unmount()
      client.clear()
    }
  })

  test('keeps polling after a failed installation check', async () => {
    const client = new QueryClient()
    let requests = 0
    const observer = new QueryObserver(client, {
      ...gitInstallationPollingOptions([provider]),
      queryKey: ['installations'],
      initialData: [] as ConnectionResponse[],
      staleTime: Infinity,
      retry: false,
      queryFn: async () => {
        requests += 1
        if (requests === 1) throw new Error('Temporary connection failure')
        return [installation]
      },
    })
    const unsubscribe = observer.subscribe(() => {})
    try {
      await eventually(() =>
        expect(observer.getCurrentResult().isError).toBe(true)
      )
      await eventually(() =>
        expect(observer.getCurrentResult().data).toEqual([installation])
      )
      expect(requests).toBe(2)
    } finally {
      unsubscribe()
      client.clear()
    }
  })

  test('continues polling for both GitHub App auth names and existing idle accounts', () => {
    for (const auth_method of ['app', 'github_app']) {
      expect(
        gitInstallationPollingOptions([
          { ...provider, auth_method },
        ]).refetchInterval({
          state: { data: [installation] },
        })
      ).toBe(2000)
    }
    expect(
      gitInstallationPollingOptions([
        { ...provider, auth_method: 'pat' },
      ]).refetchInterval({
        state: { data: [installation] },
      })
    ).toBe(false)
    expect(
      gitInstallationPollingOptions(undefined).refetchInterval({
        state: { data: [{ ...installation, syncing: true }] },
      })
    ).toBe(2000)
  })
})

test('installation completion ignores old connections and matches the callback installation ID', () => {
  expect(hasNewGitHubInstallation(undefined, [], null)).toBe(false)
  expect(
    hasNewGitHubInstallation([installation], [installation.id], null)
  ).toBe(false)
  expect(
    hasNewGitHubInstallation(
      [{ ...installation, installation_id: null }],
      [],
      null
    )
  ).toBe(false)
  expect(hasNewGitHubInstallation([installation], [], null)).toBe(true)
  expect(hasNewGitHubInstallation([installation], [], '789')).toBe(false)
  expect(
    hasNewGitHubInstallation([installation], [installation.id], '456')
  ).toBe(true)
})
