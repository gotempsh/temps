// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import type { UserResponse } from '@/api/client/types.gen'
import { AiAssistantProvider } from '@/components/ai/AiAssistantContext'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { TEMPS_CLI_VERSION } from '@/lib/ai-onboarding'
import type { HandoffSubject } from '@/lib/agent-handoff'
import { installBrowserWindow } from '@/test/dom'
import { AgentHandoffButton } from './AgentHandoffButton'
import {
  PageSubjectStoreContext,
  createPageSubjectStore,
  type PageSubjectStore,
} from './page-subject'

installBrowserWindow()

// `Request` in the test runtime needs an absolute URL; the console's client
// is configured with a relative one.
const realFetch = globalThis.fetch
const realBaseUrl = client.getConfig().baseUrl
let requested: string[] = []
beforeEach(() => {
  client.setConfig({ baseUrl: 'http://localhost/api' })
})
afterEach(() => {
  cleanup()
  globalThis.fetch = realFetch
  client.setConfig({ baseUrl: realBaseUrl })
  requested = []
})

const TRACE: HandoffSubject = {
  kind: 'trace',
  project: { id: 4, slug: 'storefront', name: 'storefront' },
  traceId: '4bf92f3577b34da6a3ce929d0e0e4736',
}

type Settings = { external_url?: string | null }

function setup(options: {
  store?: PageSubjectStore
  /** `undefined` leaves the settings read in flight. */
  settings?: Settings | 'refused'
  role?: string
}) {
  const store = options.store ?? createPageSubjectStore()
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  // The panel reads the address through the real generated query, so the
  // three cases are three answers from the server, not three seeded caches.
  const settings = options.settings
  globalThis.fetch = ((input: RequestInfo | URL) => {
    requested.push(input instanceof Request ? input.url : String(input))
    if (settings === undefined) return new Promise<Response>(() => {})
    if (settings === 'refused') {
      // What a person without `settings:read` gets.
      return Promise.resolve(
        new Response(
          JSON.stringify({ title: 'Insufficient permissions', status: 403 }),
          {
            status: 403,
            headers: { 'Content-Type': 'application/problem+json' },
          }
        )
      )
    }
    return Promise.resolve(
      new Response(JSON.stringify(settings), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      })
    )
  }) as typeof fetch
  const user = {
    id: 1,
    email: 'ada@example.com',
    role: options.role ?? 'admin',
  } as unknown as UserResponse

  function Shell({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={client}>
        <AuthContext.Provider
          value={{
            user,
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <AiAssistantProvider>
            <PageSubjectStoreContext.Provider value={store}>
              <MemoryRouter
                initialEntries={['/projects/storefront/traces/4bf9']}
              >
                {children}
              </MemoryRouter>
            </PageSubjectStoreContext.Provider>
          </AiAssistantProvider>
        </AuthContext.Provider>
      </QueryClientProvider>
    )
  }
  render(<AgentHandoffButton />, { wrapper: Shell })
  return { store, client }
}

function trigger(): HTMLElement {
  // Hidden triggers are out of the accessibility tree, so query the DOM.
  const button = document.querySelector('button[title="Hand off to agent"]')
  if (!(button instanceof HTMLElement)) throw new Error('no handoff trigger')
  return button
}

function promptText(): string {
  return screen.getByTestId('agent-handoff-prompt').textContent ?? ''
}

async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20))
  })
}

describe('AgentHandoffButton', () => {
  test('stays out of the way on a page that declared nothing', () => {
    setup({ settings: {} })
    expect(trigger().className).toContain('hidden')
    expect(screen.queryByText('Prompt')).toBeNull()
  })

  test('asks the server nothing until the panel is opened', async () => {
    const { store } = setup({ settings: {} })
    act(() => store.set('page', TRACE))
    await settle()
    expect(requested).toEqual([])

    fireEvent.click(trigger())
    await settle()
    expect(requested.map((url) => new URL(url, 'http://x').pathname)).toEqual([
      '/api/settings',
    ])
  })

  test('shows itself, by name, once a page declares what it is showing', () => {
    const { store } = setup({ settings: {} })
    act(() => store.set('page', TRACE))

    expect(trigger().className).not.toContain('hidden')
    expect(trigger().textContent).toBe('Hand off to agent')
    // The label stays for screen readers when the text is hidden on a phone.
    expect(trigger().querySelector('span')?.className).toBe('max-sm:sr-only')
  })

  test('opens a panel with the prompt readable before anything is copied', async () => {
    const { store } = setup({
      settings: { external_url: 'https://temps.example.com' },
    })
    act(() => store.set('page', TRACE))
    fireEvent.click(trigger())
    await settle()

    const text = promptText()
    expect(text).toContain('Instance:   https://temps.example.com')
    expect(text).toContain(
      'Looking at: trace 4bf92f3577b34da6a3ce929d0e0e4736 in project "storefront" (ID 4)'
    )
    expect(text).toContain(
      `bunx @temps-sdk/cli@${TEMPS_CLI_VERSION} --target-context temps-example-com traces show 4bf92f3577b34da6a3ce929d0e0e4736 --project-id 4 --json`
    )
    expect(
      screen.getByText("The address is this instance's external URL setting.")
    ).toBeTruthy()
  })

  test('the panel never grows past the room the window has, and scrolls instead', async () => {
    const { store } = setup({
      settings: { external_url: 'https://temps.example.com' },
    })
    act(() => store.set('page', TRACE))
    fireEvent.click(trigger())
    await settle()

    const panel = document.querySelector(
      '[data-radix-popper-content-wrapper] > *'
    )
    expect(panel?.className).toContain(
      'max-h-[var(--radix-popover-content-available-height)]'
    )
    expect(panel?.className).toContain('overflow-y-auto')
  })

  test('says what the sign-in grants, for how long, and that the rules are not enforced', async () => {
    const { store } = setup({ settings: {}, role: 'admin' })
    act(() => store.set('page', TRACE))
    fireEvent.click(trigger())
    await settle()

    const notice = screen.getByText(/The agent signs in as you\./)
    const text = notice.textContent ?? ''
    expect(text).toContain('a key with your whole role (admin) for 90 days')
    expect(text).toContain('It can do anything you can do here.')
    expect(text).toContain(
      'The rules in the prompt are instructions to the agent; this server does not enforce them.'
    )
    expect(
      screen.getByRole('link', { name: 'API keys' }).getAttribute('href')
    ).toBe('/settings/keys')
  })

  test('the request box becomes the last line of the prompt', async () => {
    const { store } = setup({ settings: {} })
    act(() => store.set('page', TRACE))
    fireEvent.click(trigger())
    await settle()

    const box = screen.getByLabelText('What do you want the agent to do?')
    fireEvent.change(box, {
      target: { value: 'explain what went wrong in this request.' },
    })

    const lines = promptText().split('\n')
    expect(lines[lines.length - 1]).toBe(
      'What I want: explain what went wrong in this request.'
    )
  })

  test('falls back to the address bar when the person may not read settings', async () => {
    const { store } = setup({ settings: 'refused' })
    act(() => store.set('page', TRACE))
    fireEvent.click(trigger())
    await settle()

    expect(promptText()).toContain(`Instance:   ${window.location.origin}`)
    expect(screen.getByText(/The address is this browser tab's\./)).toBeTruthy()
  })

  test('shows a skeleton and offers nothing to copy until the address is known', async () => {
    const { store } = setup({})
    act(() => store.set('page', TRACE))
    fireEvent.click(trigger())
    await settle()

    expect(screen.queryByTestId('agent-handoff-prompt')).toBeNull()
    expect(document.querySelector('[aria-busy="true"]')).not.toBeNull()
    expect(
      screen
        .getByRole('button', { name: 'Copy prompt' })
        .hasAttribute('disabled')
    ).toBe(true)
  })

  test('the open panel follows the page as its filters change', async () => {
    const { store } = setup({ settings: {} })
    const logs = (text: string): HandoffSubject => ({
      kind: 'runtime_logs',
      mode: 'history',
      project: { id: 4, slug: 'storefront', name: 'storefront' },
      startTime: '2026-11-25T14:02:00.000Z',
      endTime: '2026-11-25T14:07:00.000Z',
      text,
    })
    act(() => store.set('page', logs('timeout')))
    fireEvent.click(trigger())
    await settle()
    expect(promptText()).toContain('--text timeout --limit 500 --json')

    act(() => store.set('page', logs('payment declined')))
    expect(promptText()).toContain(
      "--text 'payment declined' --limit 500 --json"
    )
  })

  test('closes when the page stops showing anything', async () => {
    const { store } = setup({ settings: {} })
    act(() => store.set('page', TRACE))
    fireEvent.click(trigger())
    await settle()
    expect(screen.queryByTestId('agent-handoff-prompt')).not.toBeNull()

    act(() => store.clear('page'))
    expect(screen.queryByTestId('agent-handoff-prompt')).toBeNull()
    expect(trigger().className).toContain('hidden')
  })
})
