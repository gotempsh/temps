// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

const provider = {
  id: 5,
  name: 'Primary relay',
  provider_type: 'ses',
  region: 'eu-test-1',
  is_active: true,
  credentials: {},
  created_at: '2026-02-01T00:00:00Z',
  updated_at: '2026-02-01T00:00:00Z',
}

const domain = (id: number, name: string, status: string) => ({
  id,
  domain: name,
  status,
  provider_id: 5,
  last_verified_at: status === 'verified' ? '2026-10-01T00:00:00Z' : null,
  created_at: '2026-02-01T00:00:00Z',
  updated_at: '2026-02-01T00:00:00Z',
})

const verified = domain(7, 'mail.example.com', 'verified')
const pending = domain(8, 'news.example.com', 'pending')

const emails = Array.from({ length: 3 }, (_, index) => ({
  id: `e${index}`,
  domain_id: 7,
  subject: `Your receipt ${index + 1}`,
  from_address: 'billing@mail.example.com',
  to_addresses: ['someone@example.org'],
  status: index === 2 ? 'failed' : 'sent',
  track_opens: true,
  track_clicks: true,
  open_count: 2,
  click_count: 1,
  created_at: '2026-10-20T00:00:00Z',
  sent_at: '2026-10-20T00:00:00Z',
}))

const stats = {
  total: 3,
  sent: 2,
  failed: 1,
  captured: 0,
  queued: 1,
  sending: 0,
  delivery_unknown: 0,
}

interface Setup {
  providers?: unknown[]
  domains?: unknown[]
  stats?: unknown
  emails?: unknown[]
}

const running: Setup = {
  providers: [provider],
  domains: [verified, pending],
  stats,
  emails,
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

async function mockEmail(page: Page, width: number, setup: Setup = {}) {
  await page.setViewportSize({ width, height: 1000 })
  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, '')
    let body: unknown = []
    if (path === '/user/me')
      body = {
        id: 1,
        name: 'Operator',
        username: 'operator',
        email: 'operator@example.test',
        role: 'admin',
        mfa_enabled: false,
      }
    else if (path === '/operations')
      body = { operations: [], total: 0, page: 1, per_page: 20, total_pages: 0 }
    else if (path === '/projects')
      body = { projects: [], total: 0, page: 1, per_page: 20 }
    else if (path === '/email-providers') body = setup.providers ?? []
    else if (path === '/email-domains') body = setup.domains ?? []
    else if (path === '/email-domains/7')
      body = { domain: verified, dns_records: [] }
    else if (path === '/email-domains/8')
      body = {
        domain: pending,
        dns_records: [
          {
            record_type: 'TXT',
            name: 'news.example.com',
            value: 'v=spf1 include:example.test ~all',
            status: 'pending',
          },
        ],
      }
    else if (path === '/emails/stats') body = setup.stats ?? noEmails
    else if (path === '/emails/events/stats')
      body = {
        delivered: 2,
        opened: 1,
        clicked: 1,
        bounced: 0,
        complained: 0,
        open_rate: 50,
        click_rate: 50,
        bounce_rate: 0,
      }
    else if (path === '/emails/events')
      body = {
        events: (setup.emails ?? []).length
          ? [
              {
                id: 1,
                email_id: 'e0',
                event_type: 'clicked',
                link_url: 'https://example.com/receipt',
                ip_address: '203.0.113.10',
                user_agent: 'Mozilla/5.0',
                created_at: '2026-10-20T00:00:00Z',
              },
            ]
          : [],
        total: (setup.emails ?? []).length ? 1 : 0,
        page: 1,
        page_size: 10,
      }
    else if (path === '/emails') {
      const rows = setup.emails ?? []
      body = { data: rows, total: rows.length, page: 1, page_size: 10 }
    }
    await route.fulfill({ json: body })
  })
}

test('a new install is walked through setup on one page', async ({ page }) => {
  await mockEmail(page, 1400)
  await page.goto('/email')
  const setup = page.getByRole('region', { name: 'Set up sending' })
  await expect(setup.getByText('0 of 3 done')).toBeVisible()
  await expect(setup.getByRole('link')).toHaveText(['Add provider'])
  await expect(page.getByRole('tab')).toHaveText(['Overview', 'SDK'])

  await setup.getByRole('link', { name: 'Add provider' }).click()
  await expect(page).toHaveURL(/\/email\/providers\/new$/)
})

test('domains lead, with providers beside them when there is room', async ({
  page,
}) => {
  await mockEmail(page, 1400, running)
  await page.goto('/email')
  const domains = page.getByRole('heading', { name: 'Domains', exact: true })
  const providers = page.getByRole('heading', {
    name: 'Providers',
    exact: true,
  })
  await expect(
    page.getByRole('link', { name: 'mail.example.com' })
  ).toBeVisible()
  const domainsBox = (await domains.boundingBox())!
  const providersBox = (await providers.boundingBox())!
  expect(providersBox.x).toBeGreaterThan(domainsBox.x)
  expect(providersBox.y).toBe(domainsBox.y)
  await expect(
    page.getByRole('region', { name: 'Set up sending' })
  ).toHaveCount(0)
  await expect(page.getByRole('link', { name: /Your receipt 1/ })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Clicked' })).toBeVisible()
})

test('beside a wide sidebar there is no room, so providers go under domains', async ({
  page,
}) => {
  // 1100px is wider than a window breakpoint would call narrow, but the
  // sidebar leaves the page about 750px.
  await mockEmail(page, 1100, running)
  await page.goto('/email')
  await expect(
    page.getByRole('link', { name: 'mail.example.com' })
  ).toBeVisible()
  const domainsBox = (await page
    .getByRole('heading', { name: 'Domains', exact: true })
    .boundingBox())!
  const providersBox = (await page
    .getByRole('heading', { name: 'Providers', exact: true })
    .boundingBox())!
  expect(providersBox.x).toBe(domainsBox.x)
  expect(providersBox.y).toBeGreaterThan(domainsBox.y)
})

test('nothing on the page runs past a phone screen', async ({ page }) => {
  await mockEmail(page, 390, running)
  await page.goto('/email')
  await expect(page.getByRole('link', { name: /Your receipt 1/ })).toBeVisible()
  const overflow = await page
    .locator('main')
    .evaluate((main) => main.scrollWidth - main.clientWidth)
  expect(overflow).toBeLessThanOrEqual(0)
})

test('a verified domain lists its sent emails above its DNS records', async ({
  page,
}) => {
  await mockEmail(page, 1400, running)
  await page.goto('/email/domains/7')
  const sent = page.getByRole('heading', { name: 'Sent emails', exact: true })
  await expect(sent).toBeVisible()
  await expect(page.getByRole('link', { name: /Your receipt 1/ })).toBeVisible()
  const records = page.getByText('DNS records', { exact: true })
  expect((await sent.boundingBox())!.y).toBeLessThan(
    (await records.boundingBox())!.y
  )
  await expect(page.getByRole('link', { name: 'Back to email' })).toHaveCount(0)
})

test('a domain still being verified keeps its DNS records first', async ({
  page,
}) => {
  await mockEmail(page, 1400, { ...running, emails: [] })
  await page.goto('/email/domains/8')
  const sent = page.getByRole('heading', { name: 'Sent emails', exact: true })
  await expect(sent).toBeVisible()
  const records = page.getByText('DNS records', { exact: true })
  expect((await records.boundingBox())!.y).toBeLessThan(
    (await sent.boundingBox())!.y
  )
  await expect(
    page.getByText('Nothing sent from this domain yet')
  ).toBeVisible()
})
