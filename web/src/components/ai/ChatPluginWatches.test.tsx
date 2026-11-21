// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CHAT_CONTINUE_EVENT, type ChatContinueDetail } from './chat-blueprint'
import { ChatPluginWatches } from './ChatPluginWatches'

installBrowserWindow()
afterEach(cleanup)
function mount() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
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
  let detail: ChatContinueDetail | undefined
  window.addEventListener(
    CHAT_CONTINUE_EVENT,
    (event) => {
      detail = (event as CustomEvent<ChatContinueDetail>).detail
    },
    { once: true }
  )
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
  expect(detail).toEqual({
    conversationPublicId: 'chat-1',
    message:
      'Set up a watch in this chat. Follow failed reports and summarize their errors.',
  })
  expect(view.queryByRole('textbox')).toBeNull()
})
