// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, within } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { ExternalProjectConfigurator } from './ExternalProjectConfigurator'

installBrowserWindow()
afterEach(cleanup)

function Probe() {
  return <p data-testid="path">{useLocation().pathname}</p>
}

function page() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/projects/new']}>
        <ExternalProjectConfigurator onCancel={() => {}} />
        <Probe />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30))
  })
}

test('needs a name before it can be created', async () => {
  const api = mockApi({})
  render(page())
  const view = within(document.body)

  const create = view.getByRole('button', { name: /create service/i })
  expect((create as HTMLButtonElement).disabled).toBe(true)

  await act(async () => {
    fireEvent.change(view.getByLabelText('Service name'), {
      target: { value: '  ' },
    })
  })
  expect((create as HTMLButtonElement).disabled).toBe(true)
  expect(api.calls).toEqual([])
  api.restore()
})

test('creates an external service with the trimmed name', async () => {
  const bodies: Record<string, unknown>[] = []
  const api = mockApi({
    'POST /projects': async (request) => {
      bodies.push((await request.json()) as Record<string, unknown>)
      return jsonResponse({ id: 5, slug: 'marketing-site', name: 'x' })
    },
  })
  const view = render(page())

  await act(async () => {
    fireEvent.change(within(view.container).getByLabelText('Service name'), {
      target: { value: ' marketing-site ' },
    })
  })
  await act(async () => {
    fireEvent.click(
      within(view.container).getByRole('button', { name: /create service/i })
    )
  })
  await settle()

  expect(bodies).toMatchObject([
    { name: 'marketing-site', hosting: 'external' },
  ])
  expect(view.getByTestId('path').textContent).toBe(
    '/projects/marketing-site/analytics'
  )
  api.restore()
})
