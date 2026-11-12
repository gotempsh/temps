// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DockBody } from './AiAssistantDock'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from './AiAssistantContext-shared'
import { ASSISTANT_CONTEXT_TYPE } from './assistant-state'

installBrowserWindow()
// The dock reopens the chat it last remembered (`temps.ai.activeChat` in
// localStorage), and storage outlives a test file: start from none.
beforeEach(() => localStorage.clear())

afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
  spyOn(toast, 'success').mockRestore()
})

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

const TITLE = 'Deploy question'
const REASON = 'Missing permission AiChatWrite'
const PROBLEM = { title: 'Forbidden', detail: REASON }

/** A chat in project 1, or -- with `project_id: null` -- one without a project. */
const chat = (project_id: number | null) => ({
  public_id: 'chat-1',
  title: TITLE,
  project_id,
  context_type: project_id == null ? ASSISTANT_CONTEXT_TYPE : 'project',
  context_id: '1',
  kind: 'project',
  status: 'active',
  ai_model: 'm',
  ai_permission_mode: 'p',
  ai_provider: 'x',
  created_at: '2026-11-01T10:00:00Z',
  last_activity_at: '2026-11-01T10:00:00Z',
})

function setup(
  projectId: number | null,
  routes: Parameters<typeof mockApi>[0]
) {
  const api = mockApi({
    'GET /ai/conversations': () => jsonResponse([chat(projectId)]),
    ...routes,
  })
  const success = spyOn(toast, 'success').mockImplementation(() => 'id')
  const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
  render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <MemoryRouter initialEntries={['/chat']}>
        <AiAssistantContext.Provider value={assistant}>
          <DockBody layout="page" onClose={() => {}} />
        </AiAssistantContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, success, failure }
}

async function deleteChat() {
  const trigger = await screen.findByRole('button', { name: 'Delete chat' })
  await act(async () => {
    fireEvent.click(trigger)
  })
  const confirm = await screen.findByRole('button', { name: /^Delete$/ })
  await act(async () => {
    fireEvent.click(confirm)
  })
}

async function renameChat(title: string) {
  const trigger = await screen.findByRole('button', { name: 'Rename chat' })
  await act(async () => {
    fireEvent.click(trigger)
  })
  const input = (await screen.findByDisplayValue(TITLE)) as HTMLInputElement
  await act(async () => {
    fireEvent.change(input, { target: { value: title } })
  })
  await act(async () => {
    fireEvent.submit(input.closest('form') as HTMLFormElement)
  })
}

for (const [scope, projectId, archive, patch] of [
  [
    'project',
    1,
    'POST /projects/1/ai/conversations/chat-1/archive',
    'PATCH /projects/1/ai/conversations/chat-1',
  ],
  [
    'project-less',
    null,
    'POST /ai/conversations/chat-1/archive',
    'PATCH /ai/conversations/chat-1',
  ],
] as const) {
  test(`a refused delete of a ${scope} chat keeps it and gives the reason`, async () => {
    const { success, failure } = setup(projectId, {
      [archive]: () => jsonResponse(PROBLEM, 403),
    })
    await deleteChat()

    await waitFor(() =>
      expect(failure).toHaveBeenCalledWith('Could not delete chat', {
        description: REASON,
      })
    )
    expect(success).not.toHaveBeenCalled()
    expect(screen.queryAllByText(TITLE).length).toBeGreaterThan(0)
  })

  test(`a refused rename of a ${scope} chat keeps the old title and gives the reason`, async () => {
    const { success, failure } = setup(projectId, {
      [patch]: () => jsonResponse(PROBLEM, 403),
    })
    await renameChat('Renamed locally')

    await waitFor(() =>
      expect(failure).toHaveBeenCalledWith('Could not rename chat', {
        description: REASON,
      })
    )
    expect(success).not.toHaveBeenCalled()
    expect(screen.queryAllByText('Renamed locally').length).toBe(0)
  })
}

test('a delete the server accepts is announced and the chat leaves the list', async () => {
  const { success, failure } = setup(1, {
    'POST /projects/1/ai/conversations/chat-1/archive': () =>
      new Response(null, { status: 204 }),
  })
  await deleteChat()

  await waitFor(() => expect(success).toHaveBeenCalledWith('Chat deleted'))
  expect(failure).not.toHaveBeenCalled()
  await waitFor(() => expect(screen.queryAllByText(TITLE).length).toBe(0))
})

test('a rename the server accepts shows the title it stored', async () => {
  const { success, failure } = setup(1, {
    'PATCH /projects/1/ai/conversations/chat-1': () =>
      jsonResponse({ ...chat(1), title: 'Stored by server' }),
  })
  await renameChat('Typed title')

  await waitFor(() => expect(success).toHaveBeenCalledWith('Chat renamed'))
  expect(failure).not.toHaveBeenCalled()
  await screen.findByText('Stored by server')
})
