// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { restoreApiClient } from '@/test/api-client'
import { installBrowserWindow } from '@/test/dom'
import { AiAssistantGate } from './AiAssistantGate'

installBrowserWindow()

const originalConfig = client.getConfig()

afterEach(() => {
  cleanup()
  restoreApiClient(originalConfig)
})

const activeKey = {
  api_key_masked: 'sk-…test',
  base_url: null,
  created_at: '2026-08-05T00:00:00Z',
  default_model: 'example-model',
  display_name: 'Example',
  id: 1,
  is_active: true,
  provider: 'openrouter',
  updated_at: '2026-08-05T00:00:00Z',
}

const capability = (configured: boolean, reason: string | null = null) => ({
  configured,
  reason,
  setup_path: '/settings/cloud',
  model: configured ? 'managed-model' : null,
})

/**
 * Answer the gate's three reads. A handler returns the response for its path;
 * the provider-status read is answered with an unconfigured gateway, which is
 * what the onboarding copy is built from.
 */
function serve(routes: { providers: () => Response; cloud: () => Response }) {
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      (input: RequestInfo | URL) => {
        const url = input instanceof Request ? input.url : String(input)
        const path = new URL(url).pathname
        if (path === '/api/ai/providers') {
          return Promise.resolve(routes.providers())
        }
        if (path === '/api/cloud/ai/capability') {
          return Promise.resolve(routes.cloud())
        }
        if (path === '/api/ai/provider-status') {
          return Promise.resolve(
            Response.json({
              available_providers: [],
              configured: false,
              gateway_available: false,
              reason: null,
              setup_path: null,
              summary_preference: {},
              supports_interactive_tools: false,
            })
          )
        }
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
}

function renderGate() {
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={queries}>
      <MemoryRouter>
        <AiAssistantGate onClose={() => {}}>
          <p>assistant chat</p>
        </AiAssistantGate>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('opens the assistant when a local provider key is active', async () => {
  serve({
    providers: () => Response.json([activeKey]),
    cloud: () => Response.json(capability(false, 'Managed AI is not enabled.')),
  })
  renderGate()

  await screen.findByText('assistant chat')
  expect(screen.queryByText('Get things running on Temps by chat')).toBeNull()
})

test('opens the assistant when managed AI is available', async () => {
  serve({
    providers: () => Response.json([]),
    cloud: () => Response.json(capability(true)),
  })
  renderGate()

  await screen.findByText('assistant chat')
  expect(screen.queryByText('Get things running on Temps by chat')).toBeNull()
})

test('a linked but unentitled instance onboards instead of opening the chat', async () => {
  serve({
    providers: () => Response.json([]),
    cloud: () =>
      Response.json(
        capability(false, 'Your Cloud plan does not include managed AI.')
      ),
  })
  renderGate()

  await screen.findByText('Get things running on Temps by chat')
  expect(screen.queryByText('assistant chat')).toBeNull()
  expect(
    screen
      .getByRole('link', { name: /Use Temps Cloud managed AI instead/ })
      .getAttribute('href')
  ).toBe('/settings/cloud')
  expect(
    screen.getByRole('link', { name: /Open AI provider settings/ })
  ).toBeTruthy()
})

test('a failed capability check explains itself and recovers on retry', async () => {
  let available = false
  serve({
    providers: () => Response.json([]),
    cloud: () =>
      available
        ? Response.json(capability(false, 'Managed AI is not enabled.'))
        : Response.json(
            {
              title: 'Managed AI unavailable',
              status: 503,
              detail: 'The managed capability check timed out.',
            },
            { status: 503 }
          ),
  })
  renderGate()

  await screen.findByText('AI availability is unknown')
  expect(
    screen.getByText('The managed capability check timed out.')
  ).toBeTruthy()
  expect(
    screen
      .getByRole('link', { name: 'Configure my own provider' })
      .getAttribute('href')
  ).toBe('/ai-gateway')

  available = true
  fireEvent.click(screen.getByRole('button', { name: 'Check again' }))
  await waitFor(() =>
    expect(screen.getByText('Get things running on Temps by chat')).toBeTruthy()
  )
  expect(screen.queryByText('assistant chat')).toBeNull()
})
