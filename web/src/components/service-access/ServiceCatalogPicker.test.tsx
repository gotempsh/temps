// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { MemoryRouter } from 'react-router'
import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { mockApi, jsonResponse } from '@/test/fetch-mock'
import { ServiceCatalogPicker } from './ServiceCatalogPicker'
installBrowserWindow()
afterEach(cleanup)

test('catalog paginates, resets search and explains unsupported credentials', async () => {
  const requests: URL[] = []
  const selected: string[] = []
  const api = mockApi({
    'GET /service-access/catalog': (request) => {
      const url = new URL(request.url)
      requests.push(url)
      const page = Number(url.searchParams.get('page'))
      const search = url.searchParams.get('search')
      return jsonResponse({
        items: search
          ? [
              {
                id: 'deepseek',
                name: 'DeepSeek',
                category: 'AI models',
                supported: true,
              },
            ]
          : page === 2
            ? [
                {
                  id: 'fixture',
                  name: 'Another service',
                  supported: true,
                  category: 'Developer tools',
                },
              ]
            : [
                {
                  id: 'anthropic',
                  name: 'Anthropic',
                  supported: false,
                  category: 'AI models',
                  reason: 'Requires a custom credential header',
                },
              ],
        total: search ? 1 : 21,
        page,
        page_size: 20,
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    const ui = render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <ServiceCatalogPicker
            selectedId="custom"
            disabled={false}
            onSelect={(entry) => selected.push(entry.id)}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    await waitFor(() => expect(ui.getByText('Anthropic')).toBeTruthy())
    expect(
      (ui.getByRole('button', { name: /Anthropic/ }) as HTMLButtonElement)
        .disabled
    ).toBe(false)
    expect(ui.getByText('Not supported yet')).toBeTruthy()
    fireEvent.click(ui.getByRole('button', { name: /Anthropic/ }))
    fireEvent.click(ui.getByRole('button', { name: 'Next page' }))
    await waitFor(() => expect(ui.getByText('Another service')).toBeTruthy())
    fireEvent.change(ui.getByRole('textbox', { name: 'Search services' }), {
      target: { value: 'deepseek' },
    })
    await waitFor(() => expect(ui.getByText('DeepSeek')).toBeTruthy())
    expect(requests[requests.length - 1]?.searchParams.get('page')).toBe('1')
    fireEvent.click(ui.getByRole('button', { name: /DeepSeek/ }))
    fireEvent.click(ui.getByRole('button', { name: /Custom HTTP service/ }))
    expect(selected).toEqual(['anthropic', 'deepseek', 'custom'])
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

test('search keeps results mounted, debounces typing and ignores late responses', async () => {
  const pending = new Map<string, (response: Response) => void>()
  const searches: string[] = []
  const response = (name: string) =>
    jsonResponse({
      items: [{ id: name, name, category: 'Search', supported: true }],
      total: 1,
      page: 1,
      page_size: 20,
    })
  const api = mockApi({
    'GET /service-access/catalog': (request) => {
      const search = new URL(request.url).searchParams.get('search') ?? ''
      searches.push(search)
      return search
        ? new Promise<Response>((resolve) => pending.set(search, resolve))
        : response('Initial service')
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    const ui = render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <ServiceCatalogPicker
            selectedId=""
            disabled={false}
            onSelect={() => {}}
          />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const initial = await ui.findByRole('button', { name: /Initial service/ })
    const input = ui.getByRole('textbox', { name: 'Search services' })
    fireEvent.change(input, { target: { value: 'a' } })
    fireEvent.change(input, { target: { value: 'ai' } })
    expect(ui.getByRole('button', { name: /Initial service/ })).toBe(initial)
    await waitFor(() => expect(pending.has('ai')).toBe(true))
    expect(searches).toEqual(['', 'ai'])
    expect(ui.getByRole('button', { name: /Initial service/ })).toBe(initial)
    expect(ui.getByRole('status').textContent).toBe('Updating services…')
    fireEvent.change(input, { target: { value: 'final' } })
    await waitFor(() => expect(pending.has('final')).toBe(true))
    await act(async () => pending.get('final')?.(response('Final service')))
    await ui.findByRole('button', { name: /Final service/ })
    await act(async () => pending.get('ai')?.(response('Obsolete service')))
    expect(ui.queryByRole('button', { name: /Obsolete service/ })).toBeNull()
    expect(ui.getByRole('button', { name: /Final service/ })).toBeTruthy()
    expect(input).toBe(ui.getByRole('textbox', { name: 'Search services' }))
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
