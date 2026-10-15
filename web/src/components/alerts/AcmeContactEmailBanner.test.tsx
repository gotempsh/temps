// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { AcmeContactEmailBanner } from './AcmeContactEmailBanner'

function render(settings: unknown, path = '/projects') {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  if (settings !== undefined) {
    client.setQueryData(['platform-settings'], settings)
  }
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <AcmeContactEmailBanner />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const running = {
  on_demand_tls: { enabled: true, zone: null },
  external_url: 'https://203-0-113-7.sslip.io',
  preview_domain: '203-0-113-7.sslip.io',
  letsencrypt: { email: null, environment: 'production' },
}

test('asks for the contact email, inline, while on-demand TLS would run', () => {
  const html = render(running)
  expect(html).toContain('role="alert"')
  expect(html).toContain('Set a Let&#x27;s Encrypt contact email')
  expect(html).toContain('aria-label="Let&#x27;s Encrypt contact email"')
  expect(html).toContain('Save email')
  expect(html).toContain('href="/settings"')
})

test('works for a custom base domain with no explicit zone', () => {
  const html = render({
    ...running,
    external_url: 'https://paas.example.com',
    preview_domain: 'apps.example.com',
  })
  expect(html).toContain('Save email')
})

test('is hidden once an email is set', () => {
  expect(
    render({ ...running, letsencrypt: { email: 'ops@example.com' } })
  ).toBe('')
})

test('is hidden when on-demand TLS is off', () => {
  expect(
    render({ ...running, on_demand_tls: { enabled: false, zone: null } })
  ).toBe('')
})

test('is hidden when nothing would be issued anyway (loopback install)', () => {
  expect(
    render({
      ...running,
      external_url: 'http://localhost:3000',
      preview_domain: 'localho.st',
    })
  ).toBe('')
})

test('is hidden while settings are unreadable or loading', () => {
  expect(render(undefined)).toBe('')
})

test('is hidden on the settings page, which warns on the field itself', () => {
  expect(render(running, '/settings')).toBe('')
})
