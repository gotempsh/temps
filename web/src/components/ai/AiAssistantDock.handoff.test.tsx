// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import { DockBody } from './AiAssistantDock'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from './AiAssistantContext-shared'
import type { AiChatContext } from './AiAssistantContext-shared'

installBrowserWindow()

const assistant: AiAssistantValue = {
  isOpen: true,
  projectId: 1,
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

// What a page's "Try in chat" hands to /chat: a new project chat with a prompt.
const handoff: AiChatContext = {
  contextType: 'project',
  contextId: 'synthetic-try',
  title: 'Synthetic prompt',
  startPrompt: 'Summarise the last deployment.',
  projectSlug: 'synthetic',
  projectName: 'Synthetic',
}

const originalConfig = client.getConfig()
afterEach(() => {
  cleanup()
  client.setConfig(originalConfig)
})

/** Answers the chat list; everything else stays pending. */
function serve(paths: string[]) {
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      (input: RequestInfo | URL) => {
        const url = input instanceof Request ? input.url : String(input)
        const path = new URL(url).pathname.replace(/^\/api/, '')
        paths.push(path)
        if (path === '/ai/conversations')
          return Promise.resolve(Response.json([]))
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
}

function renderDock(layout: 'page' | 'dock') {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/chat']}>
        <AiAssistantContext.Provider value={assistant}>
          <DockBody
            layout={layout}
            initialContext={handoff}
            onClose={() => {}}
          />
        </AiAssistantContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a chat handed to the full-screen page still loads the chat list beside it', async () => {
  const paths: string[] = []
  serve(paths)
  renderDock('page')

  await waitFor(() => expect(paths).toContain('/ai/conversations'))
})

test('a contextual dock chat opens on that chat alone, without the list', async () => {
  const paths: string[] = []
  serve(paths)
  renderDock('dock')

  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(paths).not.toContain('/ai/conversations')
})
