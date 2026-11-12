// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { useState } from 'react'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { restoreClientConfig } from '@/test/client-config'
import { installBrowserWindow } from '@/test/dom'
import {
  SelfHostedProviderFields,
  type SelfHostedDiscovery,
} from './SelfHostedProviderFields'
import { DEFAULT_SELF_HOSTED_CAPABILITIES } from './self-hosted-provider'

installBrowserWindow()

const originalConfig = client.getConfig()

afterEach(() => {
  cleanup()
  restoreClientConfig(client, originalConfig)
})

/** Answer model discovery with `answer`, recording each request body. */
function serveDiscovery(answer: () => Response) {
  const requests: unknown[] = []
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async (input: RequestInfo | URL) => {
        const request = input instanceof Request ? input : null
        const url = request ? request.url : String(input)
        if (new URL(url).pathname === '/api/ai/providers/discover') {
          requests.push(request ? await request.json() : null)
          return answer()
        }
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
  return requests
}

function Form({
  baseUrl,
  apiKey,
  committedBaseUrl,
}: {
  baseUrl: string
  apiKey: string
  committedBaseUrl?: string
}) {
  const [discovery, setDiscovery] = useState<SelfHostedDiscovery | null>(null)
  const [defaultModel, setDefaultModel] = useState('')
  return (
    <SelfHostedProviderFields
      open
      baseUrl={baseUrl}
      committedBaseUrl={committedBaseUrl}
      apiKey={apiKey}
      trustPrivateAddress={false}
      onTrustPrivateAddressChange={() => {}}
      canTrustPrivateAddress
      discovery={discovery}
      onDiscovered={setDiscovery}
      defaultModel={defaultModel}
      onDefaultModelChange={setDefaultModel}
      capabilities={DEFAULT_SELF_HOSTED_CAPABILITIES}
      onCapabilitiesChange={() => {}}
    />
  )
}

function renderForm(baseUrl: string, apiKey = '', committedBaseUrl?: string) {
  const queries = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={queries}>
      <MemoryRouter>
        <Form
          baseUrl={baseUrl}
          apiKey={apiKey}
          committedBaseUrl={committedBaseUrl}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a complete base URL loads the models without a click', async () => {
  const requests = serveDiscovery(() =>
    Response.json({
      models: [
        { id: 'model-a', name: 'Model A Flash' },
        { id: 'model-b', name: null },
      ],
    })
  )
  renderForm('https://llm.example.test/v1')

  await screen.findByText(/Connected: 2 models found/, undefined, {
    timeout: 3000,
  })
  expect(requests).toHaveLength(1)
  expect(requests[0]).toMatchObject({
    base_url: 'https://llm.example.test/v1',
  })
  // The first model becomes the default, shown by the name the server gave.
  expect(screen.getByRole('combobox').textContent).toContain('Model A Flash')
})

test('a server that cannot be listed shows why, once', async () => {
  const requests = serveDiscovery(() =>
    Response.json(
      {
        title: 'Discovery Failed',
        status: 502,
        detail: 'The server at llm.example.test refused the connection.',
      },
      { status: 502 }
    )
  )
  renderForm('https://llm.example.test/v1')

  const alert = await screen.findByRole('alert', undefined, { timeout: 3000 })
  expect(alert.textContent).toContain('refused the connection')
  expect(screen.getByRole('button', { name: /Reload models/ })).toBeTruthy()
  // The failure is shown, not retried in a loop.
  await new Promise((resolve) => setTimeout(resolve, 900))
  expect(requests).toHaveLength(1)
})

test('a half-typed URL sends no request', async () => {
  const requests = serveDiscovery(() => Response.json({ models: [] }))
  renderForm('https://')

  await new Promise((resolve) => setTimeout(resolve, 900))
  expect(requests).toHaveLength(0)
  expect(screen.getByText('Enter the base URL to load its models')).toBeTruthy()
})

test('a typed API key is not sent to a base URL still being typed', async () => {
  const requests = serveDiscovery(() =>
    Response.json({ models: [{ id: 'model-a', name: null }] })
  )
  // A pause on a prefix of the intended URL can name someone else's host.
  renderForm('https://llm.example.co', 'sk-test-key')

  await new Promise((resolve) => setTimeout(resolve, 900))
  expect(requests).toHaveLength(0)

  fireEvent.click(screen.getByRole('button', { name: /Load models/ }))
  await screen.findByText(/Connected: 1 model found/, undefined, {
    timeout: 3000,
  })
  expect(requests).toEqual([
    expect.objectContaining({
      api_key: 'sk-test-key',
      base_url: 'https://llm.example.co',
    }),
  ])
})

test('with an API key, a committed base URL loads the models without a click', async () => {
  const requests = serveDiscovery(() =>
    Response.json({ models: [{ id: 'model-a', name: null }] })
  )
  renderForm(
    'https://llm.example.test/v1',
    'sk-test-key',
    'https://llm.example.test/v1'
  )

  await screen.findByText(/Connected: 1 model found/, undefined, {
    timeout: 3000,
  })
  expect(requests).toEqual([
    expect.objectContaining({
      api_key: 'sk-test-key',
      base_url: 'https://llm.example.test/v1',
    }),
  ])
})

test('with an API key, editing past the committed base URL waits again', async () => {
  const requests = serveDiscovery(() => Response.json({ models: [] }))
  renderForm('https://llm.example.co', 'sk-test-key', 'https://llm.example.c')

  await new Promise((resolve) => setTimeout(resolve, 900))
  expect(requests).toHaveLength(0)
  expect(
    screen.getByText('Models load once you finish the base URL')
  ).toBeTruthy()
})
