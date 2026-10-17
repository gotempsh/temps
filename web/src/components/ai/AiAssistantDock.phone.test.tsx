// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import { DockBody } from './AiAssistantDock'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from './AiAssistantContext-shared'

installBrowserWindow()

const PHONE = 390
const DESKTOP = 1280
const DESKTOP_MIN = 768

const conversations = ['one', 'two'].map((id, i) => ({
  ai_model: 'm',
  ai_permission_mode: 'default',
  ai_provider: 'p',
  context_id: `ctx-${id}`,
  context_type: 'project',
  created_at: '2026-01-01T00:00:00Z',
  kind: 'ai',
  last_activity_at: `2026-01-0${2 - i}T00:00:00Z`,
  project_id: 1,
  project_name: 'Shop',
  project_slug: 'shop',
  public_id: `chat-${id}`,
  status: 'active',
  title: `Chat ${id}`,
  turn_status: 'idle',
}))

const assistant: AiAssistantValue = {
  isOpen: true,
  projectId: null,
  openSeq: 0,
  open: () => {},
  close: () => {},
  toggle: () => {},
  pageContext: null,
  setPageContext: () => {},
  currentProject: null,
  setCurrentProject: () => {},
}

// happy-dom exposes its controls on the window; the DOM lib does not type them.
const happyDOM = () =>
  (
    window as unknown as {
      happyDOM?: { setViewport(viewport: { width: number }): void }
    }
  ).happyDOM

const originalConfig = client.getConfig()
const originalMatchMedia = window.matchMedia
const mediaListeners = new Set<() => void>()

beforeEach(() => {
  // happy-dom never fires `change` on a MediaQueryList when the viewport
  // moves, so share one listener set and notify it from setViewport.
  window.matchMedia = ((query: string) => ({
    media: query,
    matches: window.innerWidth < DESKTOP_MIN,
    addEventListener: (_: string, listener: () => void) =>
      mediaListeners.add(listener),
    removeEventListener: (_: string, listener: () => void) =>
      mediaListeners.delete(listener),
  })) as unknown as typeof window.matchMedia
  // Only the chat list is answered; anything else (the conversation panel's own
  // requests) stays pending so the layout under test renders without it.
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      (input: RequestInfo | URL) => {
        const url = input instanceof Request ? input.url : String(input)
        if (new URL(url).pathname === '/api/ai/conversations') {
          return Promise.resolve(Response.json(conversations))
        }
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
})

afterEach(() => {
  cleanup()
  client.setConfig(originalConfig)
  happyDOM()?.setViewport({ width: DESKTOP })
  window.matchMedia = originalMatchMedia
  mediaListeners.clear()
})

function setViewport(width: number) {
  act(() => {
    happyDOM()?.setViewport({ width })
    mediaListeners.forEach((listener) => listener())
  })
}

async function renderPhonePage(search = '') {
  setViewport(PHONE)
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[`/chat${search}`]}>
        <AiAssistantContext.Provider value={assistant}>
          <DockBody layout="page" onClose={() => {}} />
        </AiAssistantContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByRole('button', { name: 'Show chat list' })
  // The composer focuses itself on the next animation frame after it mounts;
  // let that settle so it cannot land after the focus moves under test.
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
  return view
}

test('on a phone the chat list starts closed and opens in place of the conversation', async () => {
  await renderPhonePage('?chat=chat-one')
  expect(screen.queryByText('Chats')).toBeNull()

  const toggle = screen.getByRole('button', { name: 'Show chat list' })
  expect(toggle.getAttribute('aria-expanded')).toBe('false')
  fireEvent.click(toggle)

  expect(await screen.findByText('Chats')).toBeTruthy()
  // Focus follows the swap to the control that closes the list.
  expect(document.activeElement).toBe(
    screen.getByRole('button', { name: 'Close chat list' })
  )
})

test('closing the phone chat list returns focus to its toggle', async () => {
  await renderPhonePage('?chat=chat-one')
  fireEvent.click(screen.getByRole('button', { name: 'Show chat list' }))
  fireEvent.click(
    await screen.findByRole('button', { name: 'Close chat list' })
  )

  expect(screen.queryByText('Chats')).toBeNull()
  expect(document.activeElement).toBe(
    screen.getByRole('button', { name: 'Show chat list' })
  )
})

test('picking the chat that is already open still returns focus to the toggle', async () => {
  await renderPhonePage('?chat=chat-one')
  fireEvent.click(screen.getByRole('button', { name: 'Show chat list' }))
  const list = (await screen.findByText('Chats')).closest('aside')!
  fireEvent.click(within(list).getByText('Chat one'))

  expect(screen.queryByText('Chats')).toBeNull()
  expect(document.activeElement).toBe(
    screen.getByRole('button', { name: 'Show chat list' })
  )
})

test('resizing from a phone to desktop shows the list beside the conversation', async () => {
  await renderPhonePage('?chat=chat-one')
  expect(screen.queryByText('Chats')).toBeNull()

  setViewport(DESKTOP)

  expect(await screen.findByText('Chats')).toBeTruthy()
  expect(
    screen
      .getByRole('button', { name: 'Hide chat list' })
      .getAttribute('aria-expanded')
  ).toBe('true')
})
