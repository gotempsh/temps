// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  CHAT_CONTINUE_EVENT,
  CHAT_WATCH_DELIVERY_EVENT,
  type ChatContinueDetail,
  type WatchRefreshDetail,
} from './chat-blueprint'
import { ChatPluginWatches } from './ChatPluginWatches'

installBrowserWindow()
afterEach(cleanup)
function mount(
  client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
) {
  const view = render(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <ChatPluginWatches chat="chat-1" />
      </QueryClientProvider>
    </MemoryRouter>
  )
  fireEvent.click(view.getByRole('button', { name: /Plugin watches/ }))
  return view
}
const route = '/ai/conversations/chat-1/plugin-watches'
test('empty watch catalog points the operator to plugin settings', async () => {
  mockApi({
    ['GET ' + route]: () => jsonResponse({ sources: [], watches: [] }),
  })
  const view = mount()
  expect(
    (await view.findByRole('link', { name: 'Manage plugins' })).getAttribute(
      'href'
    )
  ).toContain('/settings/plugins')
})
test('a watch request sends the instructions to the current agent and closes the panel', async () => {
  const source = {
    plugin: 'reports',
    id: 'report.finished',
    label: 'Report finished',
    digest: 'abc',
    declaration: {
      type: 'report.finished',
      title: 'Report finished',
      filterable: ['report_id'],
      attributes_schema: {
        type: 'object',
        properties: { report_id: { type: 'integer', title: 'Report ID' } },
        required: ['report_id'],
      },
    },
  }
  mockApi({
    ['GET ' + route]: () => jsonResponse({ sources: [source], watches: [] }),
  })
  let accepted = false
  let detail: ChatContinueDetail | undefined
  const respond = (event: Event) => {
    detail = (event as CustomEvent<ChatContinueDetail>).detail
    detail.respond?.(Promise.resolve(accepted))
  }
  window.addEventListener(CHAT_CONTINUE_EVENT, respond)
  try {
    const view = mount()
    const input = await view.findByRole('textbox', {
      name: 'What should the agent watch?',
    })
    const submit = view.getByRole('button', {
      name: 'Ask agent',
    }) as HTMLButtonElement
    expect(submit.disabled).toBe(true)
    fireEvent.change(input, {
      target: { value: 'Follow failed reports and summarize their errors.' },
    })
    fireEvent.click(submit)
    expect(await view.findByRole('alert')).toBeTruthy()
    expect((input as HTMLTextAreaElement).value).toContain(
      'Follow failed reports'
    )
    accepted = true
    fireEvent.click(submit)
    await waitFor(() => expect(view.queryByRole('textbox')).toBeNull())
    expect(detail?.conversationPublicId).toBe('chat-1')
    expect(detail?.message).toBe(
      'Set up a watch in this chat. Follow failed reports and summarize their errors.'
    )
  } finally {
    window.removeEventListener(CHAT_CONTINUE_EVENT, respond)
  }
})

const watch = {
  id: 'watch-1',
  plugin: 'reports',
  source: 'changed',
  subject: '42',
  label: 'Build reports',
  mode: 'notify',
  delivered: 0,
}

test('removal names the target and sends the request only after confirmation', async () => {
  const changes: unknown[] = []
  mockApi({
    ['GET ' + route]: () => jsonResponse({ sources: [], watches: [watch] }),
    ['POST ' + route]: async (req) => {
      changes.push(await req.json())
      return new Response(null, { status: 204 })
    },
  })
  const view = mount()
  fireEvent.click(
    await view.findByRole('button', { name: 'Remove Build reports' })
  )
  expect(await view.findByRole('alertdialog')).toBeTruthy()
  expect(
    view.getByText(/Removing Build reports stops future deliveries/)
  ).toBeTruthy()
  expect(changes).toEqual([])
  fireEvent.click(view.getByRole('button', { name: 'Remove watch' }))
  await waitFor(() =>
    expect(changes).toEqual([{ action: 'remove', id: 'watch-1' }])
  )
})

test('a later poll retries a failed transcript refresh even when the delivery count is unchanged', async () => {
  let delivered = 0
  let attempts = 0
  mockApi({
    ['GET ' + route]: () =>
      jsonResponse({ sources: [], watches: [{ ...watch, delivered }] }),
  })
  const refresh = (event: Event) => {
    const { respond } = (event as CustomEvent<WatchRefreshDetail>).detail
    respond(Promise.resolve(++attempts > 1))
  }
  window.addEventListener(CHAT_WATCH_DELIVERY_EVENT, refresh)
  try {
    const cache = new QueryClient()
    const view = mount(cache)
    await view.findByRole('button', { name: 'Remove Build reports' })
    delivered = 1
    const poll = () =>
      cache.invalidateQueries({ queryKey: ['plugin-watches', 'chat-1'] })
    await poll()
    await waitFor(() => expect(attempts).toBe(1))
    await poll()
    await waitFor(() => expect(attempts).toBe(2))
    await poll()
    expect(attempts).toBe(2)
  } finally {
    window.removeEventListener(CHAT_WATCH_DELIVERY_EVENT, refresh)
  }
})

test('first poll of a new notify watch refreshes once after the initial snapshot', async () => {
  let added = false
  let refreshes = 0
  const original = { ...watch, delivered: 3 }
  const api = mockApi({
    ['GET ' + route]: () =>
      jsonResponse({
        sources: [],
        watches: added
          ? [original, { ...watch, id: 'new-watch', delivered: 1 }]
          : [original],
      }),
  })
  const refresh = (event: Event) => {
    const { respond } = (event as CustomEvent<WatchRefreshDetail>).detail
    refreshes++
    respond(Promise.resolve(true))
  }
  window.addEventListener(CHAT_WATCH_DELIVERY_EVENT, refresh)
  try {
    const cache = new QueryClient()
    const view = mount(cache)
    await view.findByRole('button', { name: 'Remove Build reports' })
    expect(refreshes).toBe(0)
    added = true
    const poll = () =>
      cache.invalidateQueries({ queryKey: ['plugin-watches', 'chat-1'] })
    await poll()
    await waitFor(() => expect(refreshes).toBe(1))
    await poll()
    expect(refreshes).toBe(1)
  } finally {
    window.removeEventListener(CHAT_WATCH_DELIVERY_EVENT, refresh)
    api.restore()
  }
})
