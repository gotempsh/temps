// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from './AiAssistantContext-shared'
import { CHAT_CONTINUE_EVENT } from './chat-blueprint'
import { DebugChatPanel } from './DebugChatPanel'

installBrowserWindow()

const CHAT = 'chat-drafts'

const assistant: AiAssistantValue = {
  isOpen: true,
  projectId: null,
  openSeq: 0,
  open: () => {},
  openPage: () => {},
  clearInitialContext: () => {},
  close: () => {},
  toggle: () => {},
  pageContext: null,
  setPageContext: () => {},
  currentProject: null,
  setCurrentProject: () => {},
}

function detail(messages: Array<{ role: string; content: string }>) {
  return {
    public_id: CHAT,
    ai_provider: 'gateway',
    ai_model: 'm',
    ai_permission_mode: 'default',
    context_type: 'assistant',
    context_id: 'assistant',
    status: 'active',
    title: 'Chat',
    turn_status: 'idle',
    created_at: '2026-01-01T00:00:00Z',
    messages: messages.map((message, index) => ({
      ...message,
      created_at: `2026-01-01T00:00:0${index}Z`,
      truncated: false,
    })),
    page: { has_more: false },
  }
}

const originalConfig = client.getConfig()
let sent: Array<{ content: string; attachments: unknown[] }> = []

/** A backend that answers the thread read and records (or refuses) sends. */
function serve(
  messages: Array<{ role: string; content: string }>,
  refuseSend = false
) {
  sent = []
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const request = input instanceof Request ? input : null
        const url = new URL(request?.url ?? String(input))
        const method = request?.method ?? init?.method ?? 'GET'
        if (url.pathname === `/api/ai/conversations/${CHAT}`) {
          return Response.json(detail(messages))
        }
        if (
          url.pathname === `/api/ai/conversations/${CHAT}/messages` &&
          method === 'POST'
        ) {
          const body = JSON.parse(
            request ? await request.text() : String(init?.body)
          )
          sent.push(body)
          if (refuseSend) {
            return Response.json(
              { title: 'Quota exceeded', status: 429, detail: 'Try later' },
              {
                status: 429,
                headers: { 'Content-Type': 'application/problem+json' },
              }
            )
          }
          return Response.json({ turn_started_at: '2026-01-01T00:01:00Z' })
        }
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
}

beforeEach(() => {
  sent = []
  // The composer keeps an unsent draft per chat; each test starts empty.
  window.localStorage.clear()
})

afterEach(() => {
  cleanup()
  client.setConfig(originalConfig)
})

function renderPanel(initialMessage?: string, initialDraft?: string) {
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <MemoryRouter>
      <QueryClientProvider client={queries}>
        <AiAssistantContext.Provider value={assistant}>
          <DebugChatPanel
            userScoped
            conversationPublicId={CHAT}
            contextType="assistant"
            contextId="assistant"
            initialMessage={initialMessage}
            initialDraft={initialDraft}
          />
        </AiAssistantContext.Provider>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

const composer = () => {
  const textarea = document.querySelector('textarea')
  if (!textarea) throw new Error('The chat has no composer')
  return textarea
}

test('carrying the chat on for the user leaves the typed draft in the composer', async () => {
  serve([
    { role: 'user', content: 'Design a task app' },
    { role: 'assistant', content: 'Here is a design.' },
  ])
  renderPanel()
  await screen.findByText('Here is a design.')

  const draft = 'and add sign-in with GitHub'
  fireEvent.change(composer(), { target: { value: draft } })
  await waitFor(() => expect(composer().value).toBe(draft))

  act(() => {
    window.dispatchEvent(
      new CustomEvent(CHAT_CONTINUE_EVENT, {
        detail: {
          conversationPublicId: CHAT,
          message: 'I accepted the architecture (version 1).',
        },
      })
    )
  })

  await waitFor(() => expect(sent).toHaveLength(1))
  expect(sent[0]?.content).toBe('I accepted the architecture (version 1).')
  expect(sent[0]?.attachments).toEqual([])
  expect(composer().value).toBe(draft)
})

test('a refused opening message goes back into the composer', async () => {
  serve([], true)
  renderPanel('Build me a todo app')

  await waitFor(() => expect(sent).toHaveLength(1))
  expect(sent[0]?.content).toBe('Build me a todo app')
  await waitFor(() => expect(composer().value).toBe('Build me a todo app'))
})

test('a prompt handed to the chat waits in the composer, unsent', async () => {
  serve([])
  renderPanel(undefined, 'Summarise the last 7 days')

  await waitFor(() =>
    expect(composer().value).toBe('Summarise the last 7 days')
  )
  expect(sent).toHaveLength(0)
})

test("the user's own saved draft wins over a handed prompt", async () => {
  serve([])
  renderPanel(undefined, 'Summarise the last 7 days')
  const typed = 'only the errors, please'
  fireEvent.change(composer(), { target: { value: typed } })
  await waitFor(() => expect(composer().value).toBe(typed))
  cleanup()

  renderPanel(undefined, 'Summarise the last 7 days')
  await waitFor(() => expect(composer().value).toBe(typed))
})
