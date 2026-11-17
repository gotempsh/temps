// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SandboxPreviewLinksCard } from './SandboxPreviewLinksCard'

installBrowserWindow()
afterEach(cleanup)

const READY = {
  sandboxId: 'sbx_1',
  hasPassword: true,
  running: true,
  destroyed: false,
  configured: true,
}

function renderCard(props: Partial<typeof READY> = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SandboxPreviewLinksCard {...READY} {...props} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('creates a link for the chosen port and lists it with its expiry', async () => {
  let body: unknown
  const api = mockApi({
    'POST /v1/sandboxes/sbx_1/preview-link': async (request) => {
      body = await request.json()
      return jsonResponse({
        url: 'https://preview.example.test/__temps/preview/login#grant=abc',
        expires_at: Math.floor(Date.now() / 1000) + 3600,
      })
    },
  })
  const view = renderCard()

  fireEvent.change(view.getByLabelText('Port'), { target: { value: '5173' } })
  fireEvent.change(view.getByLabelText('Landing path'), {
    target: { value: '/docs' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Create link' }))

  const list = await view.findByRole('list', {
    name: 'Links created in this session',
  })
  expect(list.textContent).toContain('preview.example.test')
  expect(list.textContent).toContain('Port 5173')
  expect(list.textContent).toMatch(/expires \w{3} \d{1,2},/)
  expect(list.textContent).not.toMatch(/expires in/)
  expect(body).toEqual({ port: 5173, path: '/docs', ttl_seconds: 3600 })
  api.restore()
})

test('without a preview password it explains and links to the fix', () => {
  const api = mockApi({})
  const view = renderCard({ hasPassword: false })

  expect(view.getByText(/Set a preview password first/)).toBeTruthy()
  const fix = view.getByRole('link', { name: 'Go to preview access' })
  expect(fix.getAttribute('href')).toBe('#sandbox-preview-access')
  expect(
    (view.getByRole('button', { name: 'Create link' }) as HTMLButtonElement)
      .disabled
  ).toBe(true)
  api.restore()
})

test('says when preview URLs are not configured', () => {
  const api = mockApi({})
  const unconfigured = renderCard({ configured: false })
  expect(unconfigured.getByText(/not configured on this install/)).toBeTruthy()
  // The onboarding state links straight to where the preview domain is set.
  const fix = unconfigured.getByRole('link', {
    name: 'Open platform settings',
  })
  expect(fix.getAttribute('href')).toBe('/settings')
  api.restore()
})

test('a refused link keeps the form and shows the server reason', async () => {
  const api = mockApi({
    'POST /v1/sandboxes/sbx_1/preview-link': () =>
      jsonResponse(
        { title: 'Conflict', detail: 'Sandbox has no preview password' },
        409
      ),
  })
  const view = renderCard()

  fireEvent.click(view.getByRole('button', { name: 'Create link' }))

  await view.findByText('Sandbox has no preview password')
  expect(view.queryByRole('list')).toBeNull()
  api.restore()
})

test('an out-of-range port is rejected before any request', async () => {
  const api = mockApi({})
  const view = renderCard()

  fireEvent.change(view.getByLabelText('Port'), { target: { value: '70000' } })
  fireEvent.click(view.getByRole('button', { name: 'Create link' }))

  await view.findByText('Port must be a number from 1 to 65535')
  expect(api.called('POST', '/v1/sandboxes/sbx_1/preview-link')).toBe(false)
  api.restore()
})
