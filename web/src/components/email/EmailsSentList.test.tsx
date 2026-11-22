// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EmailsSentList } from './EmailsSentList'

installBrowserWindow()
afterEach(cleanup)

const EMAIL = {
  id: 'e1',
  subject: 'Your receipt',
  from_address: 'billing@mail.example.com',
  to_addresses: ['someone@example.org'],
  status: 'sent',
  track_opens: true,
  track_clicks: false,
  open_count: 2,
  click_count: 0,
  created_at: '2026-10-12T00:00:00Z',
}

function renderList(
  domainId: number | undefined,
  respond: (url: URL) => Response
) {
  const urls: URL[] = []
  const api = mockApi({
    'GET /emails': (request) => {
      const url = new URL(request.url)
      urls.push(url)
      return respond(url)
    },
    'GET /email-domains': () => jsonResponse([]),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <EmailsSentList domainId={domainId} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, urls, view }
}

const page = (data: unknown[]) =>
  jsonResponse({ data, total: data.length, page: 1, page_size: 10 })

test('on a domain it lists only that domain, with no domain picker', async () => {
  const { api, urls, view } = renderList(7, () => page([EMAIL]))
  const link = await view.findByRole('link', { name: /Your receipt/ })
  expect(link.getAttribute('href')).toBe('/email/e1')
  expect(urls[0]?.searchParams.get('domain_id')).toBe('7')
  expect(view.queryByLabelText('Filter by domain')).toBeNull()
  expect(api.called('GET', '/email-domains')).toBe(false)
})

test('across all domains it offers the domain picker and sends no domain', async () => {
  const { urls, view } = renderList(undefined, () => page([EMAIL]))
  await view.findByRole('link', { name: /Your receipt/ })
  expect(urls[0]?.searchParams.has('domain_id')).toBe(false)
  expect(view.getByLabelText('Filter by domain')).toBeTruthy()
})

test('an empty domain says so and shows how to send', async () => {
  const { view } = renderList(7, () => page([]))
  expect(
    await view.findByText('Nothing sent from this domain yet')
  ).toBeTruthy()
  expect(
    view.getByRole('link', { name: 'See how to send' }).getAttribute('href')
  ).toBe('/email?tab=sdk')
})

test('a failed request reports the failure instead of claiming nothing was sent', async () => {
  let fail = true
  const { view } = renderList(7, () =>
    fail
      ? jsonResponse({ detail: 'Email store unavailable' }, 503)
      : page([EMAIL])
  )
  expect(await view.findByText('Email store unavailable')).toBeTruthy()
  expect(view.queryByText('Nothing sent from this domain yet')).toBeNull()

  fail = false
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  expect(await view.findByRole('link', { name: /Your receipt/ })).toBeTruthy()
})
