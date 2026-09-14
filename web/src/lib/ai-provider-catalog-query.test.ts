// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import type { ProviderCatalogDto, ProviderCatalogResponse } from '@/api/client'
import {
  aiProviderCatalogQueryOptions,
  publishVerifiedProvider,
} from './ai-provider-catalog-query'

const provider: ProviderCatalogDto = {
  id: 'claude_cli',
  name: 'Claude Code',
  install_command: '',
  auth_command: '',
  auth_flavors: [],
  models: [],
  runtime_models: [],
  permission_modes: [],
  default_permission_mode_id: 'default',
  credential_saved: false,
  credential_verification_status: 'not_saved',
  supports_max_turns: true,
  host_authenticated: false,
  model_source: 'bootstrap',
  workspace_ready: false,
}

test('publishes verified readiness to mounted observers without a refresh', async () => {
  const client = new QueryClient()
  const original: ProviderCatalogResponse = {
    default_provider: 'claude_cli',
    providers: [provider, { ...provider, id: 'codex_cli' }],
  }
  client.setQueryData(aiProviderCatalogQueryOptions.queryKey, original)
  const observer = new QueryObserver(client, {
    ...aiProviderCatalogQueryOptions,
    enabled: false,
  })
  let ready = false
  const unsubscribe = observer.subscribe((result) => {
    ready = result.data?.providers[0].workspace_ready ?? false
  })
  await publishVerifiedProvider(client, {
    ...provider,
    workspace_ready: true,
    credential_saved: true,
  })
  expect(ready).toBe(true)
  expect(
    client.getQueryData<ProviderCatalogResponse>(
      aiProviderCatalogQueryOptions.queryKey
    )?.providers[1]
  ).toEqual(original.providers[1])
  unsubscribe()
  client.clear()
})

test('an older in-flight catalog read cannot overwrite verified readiness', async () => {
  const client = new QueryClient()
  const stale: ProviderCatalogResponse = {
    default_provider: 'claude_cli',
    providers: [provider],
  }
  client.setQueryData(aiProviderCatalogQueryOptions.queryKey, stale)
  let finish: ((value: ProviderCatalogResponse) => void) | undefined
  const pending = client
    .fetchQuery({
      ...aiProviderCatalogQueryOptions,
      queryFn: () =>
        new Promise<ProviderCatalogResponse>((resolve) => {
          finish = resolve
        }),
    })
    .catch(() => undefined)
  await publishVerifiedProvider(client, {
    ...provider,
    workspace_ready: true,
    credential_saved: true,
  })
  finish?.(stale)
  await pending
  expect(
    client.getQueryData<ProviderCatalogResponse>(
      aiProviderCatalogQueryOptions.queryKey
    )?.providers[0].workspace_ready
  ).toBe(true)
  client.clear()
})
