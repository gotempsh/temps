// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { restoreApiClient } from '@/test/api-client'
import { installBrowserWindow } from '@/test/dom'
import { DockBody } from './AiAssistantDock'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from './AiAssistantContext-shared'

installBrowserWindow()

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
} as unknown as AiAssistantValue

const chat = (publicId: string, contextType: string, title: string | null) => ({
  public_id: publicId,
  context_type: contextType,
  context_id: `${contextType}-context`,
  title,
  status: 'active',
  turn_status: 'idle',
  kind: 'chat',
  project_id: null,
  ai_model: 'model',
  ai_permission_mode: 'auto',
  ai_provider: 'provider',
  created_at: '2026-01-01T00:00:00Z',
  last_activity_at: '2026-01-01T00:00:00Z',
})

const originalConfig = client.getConfig()
// A chat another test opened is remembered and would open instead of the list.
beforeEach(() => window.localStorage.clear())
afterEach(() => {
  cleanup()
  restoreApiClient(originalConfig)
})

/** Answers the chat list and the one chat; everything else stays pending. */
function serve(paths: string[], chats: ReturnType<typeof chat>[]) {
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      (input: RequestInfo | URL) => {
        const url = input instanceof Request ? input.url : String(input)
        const path = new URL(url).pathname.replace(/^\/api/, '')
        paths.push(path)
        if (path === '/ai/conversations')
          return Promise.resolve(Response.json(chats))
        const open = chats.find(
          (c) => path === `/ai/conversations/${c.public_id}`
        )
        if (open)
          return Promise.resolve(
            Response.json({ ...open, messages: [], page: {} })
          )
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
}

function renderDock(entry = '/') {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[entry]}>
        <AiAssistantContext.Provider value={assistant}>
          <DockBody layout="dock" onClose={() => {}} />
        </AiAssistantContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('chats from the retired AI workspace are listed and open through the user routes', async () => {
  const paths: string[] = []
  serve(paths, [
    chat('conv_application', 'application', 'Plan the migration'),
    chat('conv_global', 'global', 'Tidy the sandbox'),
  ])
  const view = renderDock()

  await view.findByText('Tidy the sandbox')
  fireEvent.click(view.getByText('Plan the migration'))

  await waitFor(() =>
    expect(paths).toContain('/ai/conversations/conv_application')
  )
  expect(paths.some((path) => path.startsWith('/projects/'))).toBe(false)
})

test('a chat of no kind the dock can open stays out of the list', async () => {
  serve(
    [],
    [
      chat('conv_orphan', 'deployment', 'Deployment with no project'),
      chat('conv_global', 'global', 'Tidy the sandbox'),
    ]
  )
  const view = renderDock()

  await view.findByText('Tidy the sandbox')
  expect(view.queryByText('Deployment with no project')).toBeNull()
})

test('an untitled workspace chat shows its kind, not an internal id', async () => {
  serve([], [chat('conv_global', 'global', null)])
  const view = renderDock()

  await view.findByText('Workspace chat')
  expect(view.queryByText(/global-context/)).toBeNull()
})
