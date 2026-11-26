// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import { CatalogModelsTable } from './CatalogModelsTable'
import { MODEL_CATALOG_QUERY_KEY } from './query-keys'

installBrowserWindow()
const original = client.getConfig()
afterEach(() => {
  cleanup()
  client.setConfig({ baseUrl: original.baseUrl, fetch: original.fetch })
})

function entry(index: number) {
  return {
    provider: 'openai',
    model_id: `example-model-${index}`,
    display_name: `Example ${index}`,
    input_per_million: 1,
    output_per_million: 2,
    cache_read_per_million: 0.1,
    cache_write_per_million: null,
    context_window: 200_000,
    max_output_tokens: null,
    supports_tools: null,
    supports_vision: null,
    supports_reasoning: null,
    deprecated: false,
    release_date: null,
    source_url: 'https://catalog.example.com/api.json',
    fetched_at: '2026-11-24T10:00:00Z',
  }
}

test('a refresh that shrinks the catalog moves back to a page that exists', async () => {
  let stored = Array.from({ length: 21 }, (_, index) => entry(index + 1))
  const pages: number[] = []
  client.setConfig({
    baseUrl: 'http://temps.test',
    fetch: (async (input: Request) => {
      const url = new URL(input.url)
      const page = Number(url.searchParams.get('page'))
      const size = Number(url.searchParams.get('page_size'))
      pages.push(page)
      return new Response(
        JSON.stringify({
          items: stored.slice((page - 1) * size, page * size),
          total: stored.length,
          page,
          page_size: size,
        }),
        { headers: { 'Content-Type': 'application/json' } }
      )
    }) as typeof fetch,
  })
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={queries}>
      <CatalogModelsTable />
    </QueryClientProvider>
  )
  await view.findByText('Example 1')
  // The columns a narrow card hides are repeated under the name.
  expect(view.getAllByText(/Context 200K/).length).toBeGreaterThan(0)

  fireEvent.click(view.getAllByLabelText('Next page')[0] as HTMLElement)
  await view.findByText('Example 21')

  stored = stored.slice(0, 10)
  await queries.invalidateQueries({ queryKey: MODEL_CATALOG_QUERY_KEY })
  await view.findByText('Example 1')
  await waitFor(() => expect(pages[pages.length - 1]).toBe(1))
  expect(view.queryByText(/No catalog stored yet/)).toBeNull()
})

test('a page cached before the catalog grew again is not used to clamp', async () => {
  let stored = Array.from({ length: 21 }, (_, index) => entry(index + 1))
  client.setConfig({
    baseUrl: 'http://temps.test',
    fetch: (async (input: Request) => {
      const url = new URL(input.url)
      const page = Number(url.searchParams.get('page'))
      const size = Number(url.searchParams.get('page_size'))
      return new Response(
        JSON.stringify({
          items: stored.slice((page - 1) * size, page * size),
          total: stored.length,
          page,
          page_size: size,
        }),
        { headers: { 'Content-Type': 'application/json' } }
      )
    }) as typeof fetch,
  })
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={queries}>
      <CatalogModelsTable />
    </QueryClientProvider>
  )
  await view.findByText('Example 1')
  fireEvent.click(view.getAllByLabelText('Next page')[0] as HTMLElement)
  await view.findByText('Example 21')

  // Shrink while on page 2: the table moves to page 1, and page 2 stays
  // cached with a total of 10.
  stored = stored.slice(0, 10)
  await queries.invalidateQueries({ queryKey: MODEL_CATALOG_QUERY_KEY })
  await waitFor(() => expect(view.queryByText('Example 21')).toBeNull())
  await view.findByText('Example 1')

  // Grow back and refresh on page 1; page 2's cached total is now wrong.
  stored = Array.from({ length: 21 }, (_, index) => entry(index + 1))
  await queries.invalidateQueries({ queryKey: MODEL_CATALOG_QUERY_KEY })
  const next = await waitFor(() => {
    const button = view.getAllByLabelText('Next page')[0] as HTMLElement
    expect(button.hasAttribute('disabled')).toBe(false)
    return button
  })
  fireEvent.click(next)
  await view.findByText('Example 21')
})
