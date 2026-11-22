// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AddDomain } from './AddDomain'
import { EmailDomainNew } from './EmailDomainNew'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'

installBrowserWindow()
afterEach(cleanup)

test.each([AddDomain, EmailDomainNew])(
  'domain creation uses one centered shared wizard',
  (Component) => {
    const client = new QueryClient()
    try {
      const html = renderToStaticMarkup(
        <QueryClientProvider client={client}>
          <MemoryRouter>
            <BreadcrumbProvider>
              <Component />
            </BreadcrumbProvider>
          </MemoryRouter>
        </QueryClientProvider>
      )
      expect(html).toContain('mx-auto max-w-5xl')
      expect(html).toContain('aria-label="Setup progress"')
      expect(html.match(/<h1/g)).toHaveLength(1)
      expect(html).not.toContain('overflow-auto')
      expect(html).not.toContain('space-y-6 rounded-lg border bg-card')
      expect(html).not.toContain('rounded-lg border bg-card p-4 sm:p-6')
    } finally {
      client.clear()
    }
  }
)

/** A tunnel on one domain; nothing here answers on public port 80. */
const tunnel = {
  id: 9,
  name: 'edge',
  provider: 'cloudflare_tunnel',
  domain: 'apps.example.org',
  status: 'ready',
  auto_publish: false,
  environment_hosts: false,
  edge_port: 443,
  created_at: '2026-01-01T00:00:00Z',
  nodes: [{ node_id: 3, node_name: 'worker-1', state: 'ready' }],
  publications: [],
}

const NO_PORT_80 =
  "HTTP-01 needs Let's Encrypt to reach this server on public port 80, and visitors to reach it on 443."

function methods(domain: string | null) {
  return {
    http01_available: false,
    http01: {
      configured: false,
      reason: NO_PORT_80,
      setup_path: '/settings/nodes/ingress?via=ports',
    },
    dns01_provider: {
      configured: false,
      reason: `No connected DNS provider has a verified zone covering ${domain}`,
      setup_path: '/dns-providers/add',
    },
    dns01_manual: { configured: true },
    tunnel: { configured: domain?.endsWith('.apps.example.org') ?? false },
    dns_provider: null,
    tunnels: [],
  }
}

function renderWizard() {
  const asked: (string | null)[] = []
  mockApi({
    'GET /ingress/connectors': () =>
      jsonResponse({ connectors: [tunnel], total: 1 }),
    'GET /ingress/certificate-methods': (request) => {
      const domain = new URL(request.url).searchParams.get('domain')
      asked.push(domain)
      return jsonResponse(methods(domain))
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <BreadcrumbProvider>
          <AddDomain />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, asked }
}

/** Let the tunnel and method reads settle, so the domain is matched. */
async function tunnelsLoaded(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 50))
}

async function enterDomain(
  view: ReturnType<typeof render>,
  domain: string
): Promise<void> {
  fireEvent.change(view.getByLabelText('Domain name'), {
    target: { value: domain },
  })
  fireEvent.click(view.getByRole('button', { name: 'Next' }))
  await view.findByText('How it gets HTTPS')
}

test('with a tunnel set up, a name it covers is offered both the tunnel and a certificate', async () => {
  const { view } = renderWizard()
  await tunnelsLoaded()
  await enterDomain(view, 'my-app.apps.example.org')

  expect(view.getByText('Through tunnel edge (Recommended)')).toBeTruthy()
  expect(view.getByText(/HTTP-01 Challenge/)).toBeTruthy()
  expect(view.getByText(/DNS-01 Challenge/)).toBeTruthy()
})

test('a domain the tunnel does not cover gets a certificate, DNS-01 chosen when port 80 is not reachable', async () => {
  const { view, asked } = renderWizard()
  await tunnelsLoaded()
  await enterDomain(view, 'example.com')

  // Both ways in: certificate methods, and the tunnel's onboarding.
  expect(view.getByText(/HTTP-01 Challenge/)).toBeTruthy()
  expect(view.getByText('Through a Cloudflare Tunnel')).toBeTruthy()
  expect(view.getByText(/No tunnel covers example.com yet/)).toBeTruthy()
  const checked = view
    .getAllByRole('radio')
    .filter((radio) => radio.getAttribute('aria-checked') === 'true')
  expect(checked.map((radio) => radio.getAttribute('value'))).toEqual([
    'dns-01',
  ])

  // Judged for this domain: why HTTP-01 does not fit, and how DNS-01 works.
  await view.findByText(NO_PORT_80)
  expect(asked).toContain('example.com')
  expect(view.getByText('You add one TXT record by hand')).toBeTruthy()
  expect(
    view
      .getByRole('link', { name: 'Connect a DNS provider' })
      .getAttribute('href')
  ).toBe('/dns-providers/add')
})
