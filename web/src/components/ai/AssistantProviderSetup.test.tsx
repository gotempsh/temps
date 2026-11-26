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
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import {
  AssistantProviderSetup,
  type SavedProvider,
} from './AssistantProviderSetup'

installBrowserWindow()
afterEach(() => {
  cleanup()
  restoreAllMockApis()
})

const TEST = 'POST /ai/providers/test'
const SAVE = 'POST /ai/providers'
const DISCOVER = 'POST /ai/providers/discover'

/** Mounts the setup, recording what each request carried and what it saved. */
function mount(routes: Record<string, (body: unknown) => Response>) {
  const sent: Array<{ route: string; body: unknown }> = []
  const saved: SavedProvider[] = []
  const api = mockApi(
    Object.fromEntries(
      Object.entries(routes).map(([route, answer]) => [
        route,
        async (request: Request) => {
          const body: unknown = await request.json()
          sent.push({ route, body })
          return answer(body)
        },
      ])
    )
  )
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: { queries: { retry: false } },
        })
      }
    >
      <MemoryRouter>
        <AssistantProviderSetup onSaved={(result) => void saved.push(result)} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, sent, saved, bodies: (route: string) => bodiesOf(sent, route) }
}

const bodiesOf = (
  sent: Array<{ route: string; body: unknown }>,
  route: string
) => sent.filter((call) => call.route === route).map((call) => call.body)

const type = (label: RegExp | string, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } })
const press = (name: string) =>
  fireEvent.click(screen.getByRole('button', { name }))

test('a vendor key is tried, then connected, without leaving the screen', async () => {
  const { bodies, saved } = mount({
    [TEST]: () =>
      jsonResponse({ provider: 'anthropic', success: true, latency_ms: 240 }),
    [SAVE]: () => jsonResponse({ id: 7, provider: 'anthropic' }, 201),
  })

  press('Anthropic')
  type('Anthropic API key', 'sk-ant-example')
  press('Test')

  expect(await screen.findByRole('status')).toBeTruthy()
  expect(screen.getByRole('status').textContent).toContain(
    'Anthropic answered in 240 ms'
  )
  expect(bodies(TEST)).toEqual([
    { provider: 'anthropic', api_key: 'sk-ant-example' },
  ])
  // Trying a key stores nothing.
  expect(bodies(SAVE)).toEqual([])

  press('Connect')
  await waitFor(() => expect(saved).toHaveLength(1))
  expect(bodies(SAVE)).toEqual([
    {
      provider: 'anthropic',
      display_name: 'Anthropic',
      api_key: 'sk-ant-example',
    },
  ])
  // A vendor's model is picked in the next step.
  expect(saved[0]).toEqual({ keyId: 7, modelChosen: false })
  // The whole setup is the form: nothing links to another page.
  expect(
    screen
      .queryAllByRole('link')
      .filter((link) => link.getAttribute('target') !== '_blank')
  ).toEqual([])
})

test('nothing is sent without a key', async () => {
  const { api } = mount({})

  press('Test')
  expect(await screen.findByText('Please enter an API key')).toBeTruthy()
  press('Connect')
  expect(api.calls).toEqual([])
})

test('a refused key says why, and the reason leaves once the key is edited', async () => {
  mount({
    [TEST]: () =>
      jsonResponse({
        provider: 'openrouter',
        success: false,
        latency_ms: 90,
        error: 'The key is not recognised.',
      }),
  })

  type('OpenRouter API key', 'sk-or-wrong')
  press('Test')
  expect(await screen.findByText('The key is not recognised.')).toBeTruthy()

  type('OpenRouter API key', 'sk-or-other')
  expect(screen.queryByText('The key is not recognised.')).toBeNull()
})

test('a key typed for one provider is not kept for another', () => {
  mount({})

  type('OpenRouter API key', 'sk-or-example')
  press('OpenAI')
  const field = screen.getByLabelText('OpenAI API key')
  expect((field as HTMLInputElement).value).toBe('')
})

test('OpenAI API takes its address, lists the models and connects in place', async () => {
  const { bodies, saved } = mount({
    [DISCOVER]: () =>
      jsonResponse({
        provider: 'openai_compatible',
        models: [
          { id: 'example-large', owned_by: 'example' },
          { id: 'example-small', owned_by: 'example' },
        ],
      }),
    [TEST]: () =>
      jsonResponse({
        provider: 'openai_compatible',
        success: true,
        latency_ms: 31,
      }),
    [SAVE]: () => jsonResponse({ id: 9, provider: 'openai_compatible' }, 201),
  })

  press('OpenAI API')
  // The fields are here, not behind a link to another page.
  expect(screen.queryByRole('link', { name: /Add endpoint/ })).toBeNull()
  expect(screen.getByLabelText(/^API key/)).toBeTruthy()

  // Connecting needs the address first.
  press('Connect')
  expect(
    await screen.findByText('Enter the base URL of your server.')
  ).toBeTruthy()

  type('Base URL', 'https://models.example.test/v1')
  // Listing the models is how the address is checked; it runs on its own.
  expect(
    await screen.findByText(/Connected: 2 models found/, undefined, {
      timeout: 4000,
    })
  ).toBeTruthy()
  expect(bodies(DISCOVER)).toEqual([
    {
      provider: 'openai_compatible',
      api_key: '',
      base_url: 'https://models.example.test/v1',
      trust_private_address: false,
    },
  ])

  press('Test')
  expect(await screen.findByRole('status')).toBeTruthy()
  expect(bodies(TEST)).toEqual([
    {
      provider: 'openai_compatible',
      api_key: '',
      base_url: 'https://models.example.test/v1',
      default_model: 'example-large',
      trust_private_address: false,
    },
  ])

  press('Connect')
  await waitFor(() => expect(saved).toHaveLength(1))
  expect(bodies(SAVE)).toEqual([
    {
      provider: 'openai_compatible',
      display_name: 'OpenAI API',
      api_key: '',
      base_url: 'https://models.example.test/v1',
      default_model: 'example-large',
      trust_private_address: false,
      capabilities: {
        streaming: true,
        tools: true,
        embeddings: true,
        responses_api: true,
      },
    },
  ])
  // Its model was picked in the form, so there is no second step.
  expect(saved[0]).toEqual({ keyId: 9, modelChosen: true })
})
