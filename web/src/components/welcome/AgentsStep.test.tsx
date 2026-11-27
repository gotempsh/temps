// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import type { UserResponse } from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { AgentsStep } from './AgentsStep'

installBrowserWindow()
afterEach(cleanup)

function renderStep() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: { id: 1, role: 'admin' } as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <AgentsStep onContinue={() => {}} />
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

/** Radix tabs switch on mouse down, the way a pointer selects them. */
function openTab(view: ReturnType<typeof renderStep>, name: string) {
  fireEvent.mouseDown(view.getByRole('tab', { name }), { button: 0 })
}

test('the three ways in are tabs, each with its apps and its setup', async () => {
  const api = mockApi({
    'GET /api-keys/ai-harness/status': () =>
      jsonResponse({ status: 'missing', reason: null, connected_via: null }),
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: true } }),
  })
  const view = renderStep()
  expect(view.getByRole('tablist', { name: 'What do you use?' })).toBeTruthy()
  const tabs = view.getAllByRole('tab')
  expect(tabs.map((tab) => tab.textContent?.trim())).toEqual([
    'Coding agent',
    'Claude app',
    'Another app',
  ])
  // Each tab has a mark beside its label, hidden from screen readers.
  for (const tab of tabs) {
    expect(
      tab.querySelector('[aria-hidden] svg, [aria-hidden] img')
    ).toBeTruthy()
  }
  expect(view.getByText('Paste this prompt into your agent')).toBeTruthy()

  openTab(view, 'Claude app')
  expect(
    await view.findByRole('button', { name: 'Download temps.mcpb' })
  ).toBeTruthy()
  expect(view.getByText(/Install the Temps extension/)).toBeTruthy()

  openTab(view, 'Another app')
  expect(await view.findByRole('combobox', { name: 'Which app?' })).toBeTruthy()
  expect(view.getByText('Cursor, VS Code, Windsurf or Zed.')).toBeTruthy()
  api.restore()
})

test('the Claude app tab sends Cowork and Code sessions to the coding-agent tab', async () => {
  const api = mockApi({
    'GET /api-keys/ai-harness/status': () =>
      jsonResponse({ status: 'missing', reason: null, connected_via: null }),
    'GET /settings': () => jsonResponse({ mcp_server: { enabled: true } }),
  })
  const view = renderStep()
  openTab(view, 'Claude app')
  fireEvent.click(
    await view.findByRole('button', { name: 'Show the coding-agent prompt' })
  )
  expect(
    await view.findByText('Paste this prompt into your agent')
  ).toBeTruthy()
  expect(
    view
      .getByRole('tab', { name: 'Coding agent' })
      .getAttribute('aria-selected')
  ).toBe('true')
  api.restore()
})
