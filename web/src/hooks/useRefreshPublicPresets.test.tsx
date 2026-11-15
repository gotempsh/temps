// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { detectPublicPresetsOptions } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { useRefreshPublicPresets } from './useRefreshPublicPresets'

installBrowserWindow()
afterEach(cleanup)

const ROUTE = 'GET /git/public/github/acme/demo/presets'
const OPTIONS = {
  path: { provider: 'github', owner: 'acme', repo: 'demo' },
  query: { base_url: 'https://git.example.test' },
} as Parameters<typeof useRefreshPublicPresets>[0]

function setup() {
  const client = new QueryClient()
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const { result } = renderHook(() => useRefreshPublicPresets(OPTIONS), {
    wrapper,
  })
  const cached = () =>
    client.getQueryData(detectPublicPresetsOptions(OPTIONS).queryKey)
  return { client, result, cached }
}

test('asks past the server cache and fills the query the page reads', async () => {
  let url: URL | undefined
  const fresh = { branch: 'main', presets: [] }
  mockApi({
    [ROUTE]: (request) => {
      url = new URL(request.url)
      return jsonResponse(fresh)
    },
  })
  const { result, cached } = setup()
  await act(() => result.current.mutateAsync())
  expect(url?.searchParams.get('fresh')).toBe('true')
  expect(url?.searchParams.get('base_url')).toBe('https://git.example.test')
  expect(cached()).toEqual(fresh)
})

test('a failed refresh keeps what the page already shows', async () => {
  mockApi({ [ROUTE]: () => jsonResponse({ title: 'Upstream failed' }, 502) })
  const { client, result, cached } = setup()
  const shown = { branch: 'main', presets: [] }
  client.setQueryData(detectPublicPresetsOptions(OPTIONS).queryKey, shown)
  act(() => result.current.mutate())
  await waitFor(() => expect(result.current.isError).toBe(true))
  expect(cached()).toEqual(shown)
})

test('a refresh is cached for the repository that started it', async () => {
  let answer: (response: Response) => void = () => {}
  mockApi({
    [ROUTE]: () => new Promise<Response>((resolve) => (answer = resolve)),
  })
  const client = new QueryClient()
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  const other = {
    ...OPTIONS,
    path: { provider: 'github', owner: 'acme', repo: 'other' },
  } as typeof OPTIONS
  const { result, rerender } = renderHook(
    ({ options }) => useRefreshPublicPresets(options),
    { wrapper, initialProps: { options: OPTIONS } }
  )
  act(() => result.current.mutate())
  await waitFor(() => expect(result.current.isPending).toBe(true))
  // The page moves to another repository before the answer arrives.
  rerender({ options: other })
  const fresh = { branch: 'main', presets: [] }
  answer(jsonResponse(fresh))
  await waitFor(() => expect(result.current.isSuccess).toBe(true))
  expect(
    client.getQueryData(detectPublicPresetsOptions(OPTIONS).queryKey)
  ).toEqual(fresh)
  expect(
    client.getQueryData(detectPublicPresetsOptions(other).queryKey)
  ).toBeUndefined()
})
