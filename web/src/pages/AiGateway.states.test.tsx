// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProviderKeyResponse } from '@/api/client'
import { AiGatewayPage } from './AiGateway'
import {
  aiProviderRowStatus,
  shouldPromptForFirstProviderKey,
} from './aiGatewayProviderStatus'

const keysKey: QueryKey = ['providerKeys']
const activeKey: ProviderKeyResponse = {
  id: 1,
  provider: 'openai',
  display_name: 'OpenAI',
  api_key_masked: 'sk-...abcd',
  is_active: true,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

function createClient() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  // Settings carry the external URL, so the page never reaches for `window`.
  client.setQueryData(['platform-settings'], {
    external_url: 'https://temps.example.test',
  })
  return client
}
function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  const query = client.getQueryCache().build(client, { queryKey })
  query.setState({
    ...query.state,
    status: 'error',
    error: error as Error,
    fetchStatus: 'idle',
  })
}
function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/ai-gateway']}>
        <AiGatewayPage />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const serverError = {
  title: 'Internal Server Error',
  status: 500,
  detail: 'provider key store unavailable',
}
for (const error of [serverError, new TypeError('Failed to fetch')]) {
  test(`failed provider-key read does not claim providers are unconfigured: ${String(error)}`, () => {
    const client = createClient()
    fail(client, keysKey, error)
    const html = render(client)
    expect(html).toContain('AI provider keys unavailable')
    expect(html).toContain('Retry')
    expect(html).toContain('Unknown')
    expect(html).not.toContain('Not configured')
    expect(html).not.toContain('Configure</button>')
    expect(html).not.toContain('Add a provider key')
    if (!(error instanceof TypeError)) {
      expect(html).toContain('provider key store unavailable')
    }
    client.clear()
  })
}

test('forbidden provider-key read shows access denied with the server detail', () => {
  const client = createClient()
  fail(client, keysKey, {
    title: 'Forbidden',
    status: 403,
    detail: 'Requires AiGatewayRead permission',
  })
  const html = render(client)
  expect(html).toContain('AI provider keys: access denied')
  expect(html).toContain('Requires AiGatewayRead permission')
  expect(html).not.toContain('Not configured')
  expect(html).not.toContain('Add a provider key')
  client.clear()
})

test('verified empty key list keeps the onboarding state', () => {
  const client = createClient()
  client.setQueryData(keysKey, [])
  const html = render(client)
  expect(html).toContain('Not configured')
  expect(html).toContain('Configure</button>')
  expect(html).toContain('Add a provider key')
  expect(html).not.toContain('AI provider keys unavailable')
  client.clear()
})

test('row status distinguishes unknown from not configured', () => {
  const failed = { keys: undefined, isError: true }
  expect(aiProviderRowStatus('openai', failed)).toBe('unknown')
  expect(shouldPromptForFirstProviderKey(failed, ['openai'])).toBe(false)

  const empty = { keys: [], isError: false }
  expect(aiProviderRowStatus('openai', empty)).toBe('not-configured')
  expect(shouldPromptForFirstProviderKey(empty, ['openai'])).toBe(true)

  const configured = { keys: [activeKey], isError: false }
  expect(aiProviderRowStatus('openai', configured)).toBe('active')
  expect(aiProviderRowStatus('anthropic', configured)).toBe('not-configured')
  expect(shouldPromptForFirstProviderKey(configured, ['openai'])).toBe(false)

  const disabled = {
    keys: [{ ...activeKey, is_active: false }],
    isError: false,
  }
  expect(aiProviderRowStatus('openai', disabled)).toBe('disabled')

  // Stale cache after a failed refresh: rows keep their last-known status,
  // but the first-key onboarding is not offered on unverified data.
  const stale = { keys: [], isError: true }
  expect(aiProviderRowStatus('openai', stale)).toBe('not-configured')
  expect(shouldPromptForFirstProviderKey(stale, ['openai'])).toBe(false)
})
