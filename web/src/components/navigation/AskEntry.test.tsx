// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { SidebarProvider } from '@/components/ui/sidebar'
import { rememberLastChat } from '@/lib/assistant-entry'
import { installBrowserWindow } from '@/test/dom'
import { mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { AskEntry } from './AskEntry'

installBrowserWindow()
beforeEach(() => localStorage.clear())
afterEach(() => {
  cleanup()
  restoreAllMockApis()
})

function mount(path = '/projects') {
  const api = mockApi({})
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[path]}>
        <SidebarProvider>
          <AskEntry />
        </SidebarProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, view }
}

test('Ask is a link with the assistant icon, and opens a new chat when none is on record', () => {
  const { api, view } = mount()
  const link = view.getByRole('link', { name: /^Ask/ })
  expect(link.getAttribute('href')).toBe('/chat')
  expect(link.querySelector('svg.lucide-sparkles')).toBeTruthy()
  // An entry of the menu asks the server nothing.
  expect(api.calls).toEqual([])
})

test('Ask reopens the chat last shown', () => {
  rememberLastChat('conv_7')
  const { view } = mount()
  expect(view.getByRole('link', { name: /^Ask/ }).getAttribute('href')).toBe(
    '/chat?chat=conv_7'
  )
})

test('on the assistant page Ask is the entry shown, as on any page of the menu', () => {
  expect(
    mount('/projects')
      .view.getByRole('link', { name: /^Ask/ })
      .getAttribute('aria-current')
  ).toBeNull()
  cleanup()
  expect(
    mount('/chat')
      .view.getByRole('link', { name: /^Ask/ })
      .getAttribute('aria-current')
  ).toBe('page')
})
