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
  openPage: () => {},
  clearInitialContext: () => {},
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

// The chat list that fills the screen on a phone, as opposed to the one in the
// side panel (hidden below `lg` by CSS, which happy-dom does not apply).
const inPlaceChat = (title: string) =>
  waitFor(() => {
    const chat = screen
      .getAllByText(title)
      .find((element) => !element.closest('aside[aria-label="Chats"]'))
    if (!chat) throw new Error(`"${title}" is only listed in the side panel`)
    return chat
  })

// The list beside the conversation is a resizable panel; its handle is hidden
// whenever that panel is collapsed out of the way.
const sideListShown = () =>
  !document
    .querySelector('[aria-label="Resize chat list"]')
    ?.classList.contains('hidden')

// What the chat list says above its chats when it fills the screen.
const LIST_INTRO = /Resume any AI conversation/

const backArrow = () =>
  screen.getByRole('button', { name: 'Back to all conversations' })

async function settle() {
  // The composer focuses itself on the next animation frame after it mounts;
  // let that settle before the layout changes under test.
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
}

// The page opens on the assistant's start screen, behind which the chat list
// waits. Phones have no room for both, so the back arrow swaps them.
async function renderPhonePage() {
  setViewport(PHONE)
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/chat']}>
        <AiAssistantContext.Provider value={assistant}>
          <DockBody layout="page" onClose={() => {}} />
        </AiAssistantContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await screen.findByRole('button', { name: 'Back to all conversations' })
  await settle()
  return view
}

async function openChatOnPhone(title: string) {
  fireEvent.click(backArrow())
  fireEvent.click(await inPlaceChat(title))
  await waitFor(() => expect(screen.queryByText(LIST_INTRO)).toBeNull())
  await settle()
}

test('on a phone the back arrow shows the chat list in place, without a side panel', async () => {
  await renderPhonePage()
  expect(screen.queryByText(LIST_INTRO)).toBeNull()

  fireEvent.click(backArrow())

  expect(await screen.findByText(LIST_INTRO)).toBeTruthy()
  expect(await inPlaceChat('Chat two')).toBeTruthy()
  expect(sideListShown()).toBe(false)
})

test('picking a chat on a phone opens it in place of the list', async () => {
  await renderPhonePage()
  await openChatOnPhone('Chat one')

  expect(backArrow()).toBeTruthy()
  expect(sideListShown()).toBe(false)
})

test('the back arrow on a phone returns from a chat to the list', async () => {
  await renderPhonePage()
  await openChatOnPhone('Chat one')

  fireEvent.click(backArrow())

  expect(await screen.findByText(LIST_INTRO)).toBeTruthy()
  expect(await inPlaceChat('Chat two')).toBeTruthy()
})

test('resizing from a phone to desktop shows the list beside the conversation', async () => {
  await renderPhonePage()
  await openChatOnPhone('Chat one')
  expect(sideListShown()).toBe(false)

  setViewport(DESKTOP)

  expect(sideListShown()).toBe(true)
  expect(
    within(screen.getByRole('complementary', { name: 'Chats' })).getByText(
      'Chat two'
    )
  ).toBeTruthy()
})
