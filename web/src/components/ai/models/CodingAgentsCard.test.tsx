// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import { CodingAgentsCard } from './CodingAgentsCard'

installBrowserWindow()
const original = client.getConfig()
afterEach(() => {
  cleanup()
  client.setConfig({ baseUrl: original.baseUrl, fetch: original.fetch })
})

const overview = {
  settings: { auto_update: false, updated_at: null },
  agents: [
    {
      provider_id: 'codex_cli',
      name: 'Codex',
      binary: 'codex',
      npm_package: '@openai/codex',
      latest_version: '1.1.0',
      checked_at: '2026-11-24T10:00:00Z',
      error: null,
    },
  ],
}

const notChecked = {
  provider_id: 'codex_cli',
  installed_version: null,
  latest_version: null,
  update_available: false,
  updating: false,
  checked_at: null,
  detail: null,
}

function json(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: { 'Content-Type': 'application/json' },
  })
}

test('opening the card never looks inside the workspace; Check workspace does', async () => {
  const requests: URL[] = []
  client.setConfig({
    baseUrl: 'http://temps.test',
    fetch: (async (input: Request) => {
      const url = new URL(input.url)
      requests.push(url)
      if (url.pathname.endsWith('/harness-update')) {
        return json(
          url.searchParams.get('refresh') === 'true'
            ? {
                ...notChecked,
                installed_version: '1.1.0',
                latest_version: '1.1.0',
                checked_at: '2026-11-24T10:05:00Z',
              }
            : notChecked
        )
      }
      return json(overview)
    }) as typeof fetch,
  })
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={queries}>
      <CodingAgentsCard />
    </QueryClientProvider>
  )

  await view.findByText(/Not checked yet/)
  const check = view.getByRole('button', { name: 'Check workspace' })
  const reads = requests.filter((url) =>
    url.pathname.endsWith('/harness-update')
  )
  expect(reads.length).toBeGreaterThan(0)
  for (const url of reads) {
    expect(url.searchParams.get('cached_only')).toBe('true')
    expect(url.searchParams.get('refresh')).toBeNull()
  }
  // Never read: there is nothing to update yet.
  expect(view.queryByRole('button', { name: 'Update' })).toBeNull()

  fireEvent.click(check)
  await waitFor(() => expect(view.getByText('Up to date')).toBeTruthy())
  expect(
    requests.some(
      (url) =>
        url.pathname.endsWith('/harness-update') &&
        url.searchParams.get('refresh') === 'true'
    )
  ).toBe(true)
})

test('a workspace read earlier can be checked again and a downgrade found', async () => {
  // The cache says 1.1.0 (current), but the workspace was since recreated
  // from an image with 1.0.0.
  const cachedCurrent = {
    ...notChecked,
    installed_version: '1.1.0',
    latest_version: '1.1.0',
    checked_at: '2026-11-24T10:05:00Z',
  }
  client.setConfig({
    baseUrl: 'http://temps.test',
    fetch: (async (input: Request) => {
      const url = new URL(input.url)
      if (url.pathname.endsWith('/harness-update')) {
        return json(
          url.searchParams.get('refresh') === 'true'
            ? {
                ...cachedCurrent,
                installed_version: '1.0.0',
                update_available: true,
                checked_at: '2026-11-25T09:00:00Z',
              }
            : cachedCurrent
        )
      }
      return json(overview)
    }) as typeof fetch,
  })
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={queries}>
      <CodingAgentsCard />
    </QueryClientProvider>
  )

  await waitFor(() => expect(view.getByText('Up to date')).toBeTruthy())
  expect(view.getByText(/^Checked/)).toBeTruthy()
  const update = view.getByRole('button', { name: 'Update' })
  expect(update.hasAttribute('disabled')).toBe(true)

  fireEvent.click(view.getByRole('button', { name: 'Check workspace' }))
  await waitFor(() => expect(view.getByText('1.1.0 available')).toBeTruthy())
  expect(
    view.getByRole('button', { name: 'Update' }).hasAttribute('disabled')
  ).toBe(false)
})

test('a cached read still in flight cannot undo an explicit check', async () => {
  let releaseCachedRead: (() => void) | undefined
  client.setConfig({
    baseUrl: 'http://temps.test',
    fetch: (async (input: Request) => {
      const url = new URL(input.url)
      if (!url.pathname.endsWith('/harness-update')) return json(overview)
      if (url.searchParams.get('refresh') === 'true') {
        return json({
          ...notChecked,
          installed_version: '1.0.0',
          latest_version: '1.1.0',
          update_available: true,
          checked_at: '2026-11-25T09:00:00Z',
        })
      }
      // The page's cached read: answered only after the explicit check.
      await new Promise<void>((resolve) => {
        releaseCachedRead = resolve
      })
      return json(notChecked)
    }) as typeof fetch,
  })
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={queries}>
      <CodingAgentsCard />
    </QueryClientProvider>
  )

  const check = await view.findByRole('button', { name: 'Check workspace' })
  await waitFor(() => expect(releaseCachedRead).toBeDefined())
  fireEvent.click(check)
  await waitFor(() => expect(view.getByText('1.1.0 available')).toBeTruthy())

  releaseCachedRead?.()
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(view.getByText('1.1.0 available')).toBeTruthy()
  expect(view.queryByText(/Not checked yet/)).toBeNull()
})

test('a cached read started during a check cannot undo it', async () => {
  let releaseLiveCheck: (() => void) | undefined
  let releaseLateRead: (() => void) | undefined
  let cachedReads = 0
  client.setConfig({
    baseUrl: 'http://temps.test',
    fetch: (async (input: Request) => {
      const url = new URL(input.url)
      if (!url.pathname.endsWith('/harness-update')) return json(overview)
      if (url.searchParams.get('refresh') === 'true') {
        await new Promise<void>((resolve) => {
          releaseLiveCheck = resolve
        })
        return json({
          ...notChecked,
          installed_version: '1.0.0',
          latest_version: '1.1.0',
          update_available: true,
          checked_at: '2026-11-25T09:00:00Z',
        })
      }
      cachedReads += 1
      // The first read answers at once; one started during the check is
      // held until after the check has written its result.
      if (cachedReads > 1) {
        await new Promise<void>((resolve) => {
          releaseLateRead = resolve
        })
      }
      return json(notChecked)
    }) as typeof fetch,
  })
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={queries}>
      <CodingAgentsCard />
    </QueryClientProvider>
  )

  await view.findByText(/Not checked yet/)
  fireEvent.click(view.getByRole('button', { name: 'Check workspace' }))
  await waitFor(() => expect(releaseLiveCheck).toBeDefined())
  // A refetch starts while the live check is running.
  void queries.refetchQueries({ type: 'all' })
  await waitFor(() => expect(releaseLateRead).toBeDefined())
  releaseLiveCheck?.()
  await waitFor(() => expect(view.getByText('1.1.0 available')).toBeTruthy())

  releaseLateRead?.()
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(view.getByText('1.1.0 available')).toBeTruthy()
  expect(view.queryByText(/Not checked yet/)).toBeNull()
})
