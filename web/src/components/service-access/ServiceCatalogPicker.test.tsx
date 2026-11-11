// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
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
        <ServiceCatalogPicker
          selectedId="custom"
          disabled={false}
          onSelect={(entry) => selected.push(entry.id)}
        />
      </QueryClientProvider>
    )
    await waitFor(() => expect(ui.getByText('Anthropic')).toBeTruthy())
    expect(
      (ui.getByRole('button', { name: /Anthropic/ }) as HTMLButtonElement)
        .disabled
    ).toBe(true)
    expect(ui.getByText('Requires a custom credential header')).toBeTruthy()
    fireEvent.click(ui.getByRole('button', { name: 'Next page' }))
    await waitFor(() => expect(ui.getByText('Another service')).toBeTruthy())
    fireEvent.change(ui.getByRole('textbox', { name: 'Search services' }), {
      target: { value: 'deepseek' },
    })
    await waitFor(() => expect(ui.getByText('DeepSeek')).toBeTruthy())
    expect(requests[requests.length - 1]?.searchParams.get('page')).toBe('1')
    fireEvent.click(ui.getByRole('button', { name: /DeepSeek/ }))
    fireEvent.click(ui.getByRole('button', { name: 'Custom HTTP service' }))
    expect(selected).toEqual(['deepseek', 'custom'])
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
