// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { Email } from './Email'

installBrowserWindow()
afterEach(cleanup)

const PROVIDER = {
  id: 5,
  name: 'Primary relay',
  provider_type: 'ses',
  region: 'eu-test-1',
  is_active: true,
  credentials: {},
  created_at: '2026-10-12T00:00:00Z',
  updated_at: '2026-10-12T00:00:00Z',
}
const DOMAIN = {
  id: 7,
  domain: 'mail.example.com',
  status: 'verified',
  provider_id: 5,
  created_at: '2026-10-12T00:00:00Z',
  updated_at: '2026-10-12T00:00:00Z',
}
const noEmails = {
  total: 0,
  sent: 0,
  failed: 0,
  captured: 0,
  queued: 0,
  sending: 0,
  delivery_unknown: 0,
}

function renderEmail(
  entry: string,
  data: { providers?: unknown[]; domains?: unknown[]; stats?: unknown } = {}
) {
  const api = mockApi({
    'GET /email-providers': () => jsonResponse(data.providers ?? []),
    'GET /email-domains': () => jsonResponse(data.domains ?? []),
    'GET /emails/stats': () => jsonResponse(data.stats ?? noEmails),
    'GET /emails': () =>
      jsonResponse({ data: [], total: 0, page: 1, page_size: 10 }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[entry]}>
        <BreadcrumbProvider>
          <Email />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, view }
}

test('one page shows domains before providers, then sent emails, with only the SDK apart', async () => {
  const { view } = renderEmail('/email')
  expect(
    view.getAllByRole('tab').map((tab) => tab.textContent?.trim())
  ).toEqual(['Overview', 'SDK'])

  const headings = await view.findAllByRole('heading', { level: 2 })
  const order = headings.map((heading) => heading.textContent)
  expect(order.indexOf('Domains')).toBeGreaterThan(-1)
  expect(order.indexOf('Domains')).toBeLessThan(order.indexOf('Providers'))
  expect(order.indexOf('Providers')).toBeLessThan(order.indexOf('Sent emails'))
})

for (const old of ['providers', 'domains', 'emails', 'analytics']) {
  test(`an old ?tab=${old} link lands on the overview`, () => {
    const { view } = renderEmail(`/email?tab=${old}`)
    expect(
      view.getByRole('tab', { name: 'Overview' }).getAttribute('data-state')
    ).toBe('active')
    expect(view.getByRole('heading', { name: 'Domains' })).toBeTruthy()
  })
}

test('?tab=sdk keeps opening the SDK', () => {
  const { view } = renderEmail('/email?tab=sdk')
  expect(
    view.getByRole('tab', { name: 'SDK' }).getAttribute('data-state')
  ).toBe('active')
  expect(view.queryByRole('heading', { name: 'Domains' })).toBeNull()
})

test('a new install is asked for one thing at a time, starting with a provider', async () => {
  const { view } = renderEmail('/email')
  const setup = await view.findByRole('region', { name: 'Set up sending' })
  expect(within(setup).getByText('0 of 3 done')).toBeTruthy()
  expect(
    within(setup)
      .getAllByRole('link')
      .map((link) => link.getAttribute('href'))
  ).toEqual(['/email/providers/new'])
})

test('the checklist moves on to the domain once a provider exists', async () => {
  const { view } = renderEmail('/email', { providers: [PROVIDER] })
  const setup = await view.findByRole('region', { name: 'Set up sending' })
  await waitFor(() =>
    expect(within(setup).getByText('Connected Primary relay')).toBeTruthy()
  )
  expect(
    within(setup)
      .getAllByRole('link')
      .map((link) => link.getAttribute('href'))
  ).toEqual(['/email/domains/new'])
})

test('the checklist leaves once an email has been delivered', async () => {
  const { view } = renderEmail('/email', {
    providers: [PROVIDER],
    domains: [DOMAIN],
    stats: { ...noEmails, total: 1, sent: 1 },
  })
  await view.findAllByText('mail.example.com')
  await waitFor(() => expect(view.queryByText('Set up sending')).toBeNull())
})

test('a failed lookup never claims a setup step is still to do', async () => {
  const api = mockApi({
    'GET /email-domains': () => jsonResponse([DOMAIN]),
    'GET /emails/stats': () => jsonResponse(noEmails),
    'GET /emails': () =>
      jsonResponse({ data: [], total: 0, page: 1, page_size: 10 }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/email']}>
        <BreadcrumbProvider>
          <Email />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  await view.findByText('Email data unavailable')
  expect(view.queryByText('Set up sending')).toBeNull()
  api.restore()
})
