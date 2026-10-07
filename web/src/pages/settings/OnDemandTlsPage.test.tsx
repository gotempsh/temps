// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { OnDemandTlsPage } from './OnDemandTlsPage'

function render(settings: unknown) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  client.setQueryData(['platform-settings'], settings)
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/on-demand-tls']}>
        <BreadcrumbProvider>
          <OnDemandTlsPage />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const onDemand = {
  enabled: true,
  zone: null,
  max_concurrent: 3,
  hourly_cap: 10,
  deployment_url_mode: 'http',
}

test('renders the stored state, the restart note and every control', () => {
  const html = render({
    on_demand_tls: onDemand,
    external_url: 'https://203-0-113-7.sslip.io',
    letsencrypt: { email: 'ops@example.com' },
  })
  expect(html).toContain('Saved state: on. Changes apply after Temps restarts.')
  expect(html).toContain('id="on-demand-tls-enabled"')
  expect(html).toContain('Effective zone: 203-0-113-7.sslip.io.')
  expect(html).toContain('id="on-demand-tls-max-concurrent"')
  expect(html).toContain('id="on-demand-tls-hourly-cap"')
  expect(html).toContain('Redirect to the environment URL')
  expect(html).not.toContain('Issuance will not start yet')
})

test('explains why an enabled switch would not issue, with fix links', () => {
  const html = render({
    on_demand_tls: onDemand,
    external_url: 'http://localhost:3000',
    letsencrypt: { email: null },
  })
  expect(html).toContain('Issuance will not start yet')
  expect(html).toContain('a loopback address')
  expect(html).toContain('No zone is set and none can be derived')
  expect(html).toContain('No Let&#x27;s Encrypt contact email is set')
  expect(html).toContain('Set the contact email')
  expect(html).toContain('href="/settings"')
})

test('a disabled switch shows no blocker warning', () => {
  const html = render({
    on_demand_tls: { ...onDemand, enabled: false },
    external_url: 'http://localhost:3000',
    letsencrypt: { email: null },
  })
  expect(html).toContain('Saved state: off.')
  expect(html).not.toContain('Issuance will not start yet')
})
