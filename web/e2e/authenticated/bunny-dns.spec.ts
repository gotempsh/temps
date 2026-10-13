// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Page, Route } from '@playwright/test'
import { expect, test } from '../fixtures'

/** `GET /delivery-profiles` with or without paging parameters. */
const PROFILE_LIST = /\/api\/delivery-profiles(?:\?.*)?$/

/** Answer a paginated list request with one page holding `items`. */
function fulfillPage(route: Route, items: unknown[]) {
  const query = new URL(route.request().url()).searchParams
  return route.fulfill({
    json: {
      items,
      total: items.length,
      page: Number(query.get('page') ?? 1),
      page_size: Number(query.get('page_size') ?? 20),
    },
  })
}

const provider = {
  id: 84,
  name: 'Bunny DNS demo',
  provider_type: 'bunny',
  description: 'Synthetic account',
  credentials: { api_key: '********' },
  flat_hostnames_supported: false,
  is_active: true,
  last_error: null,
  last_used_at: null,
  created_at: '2026-09-29T12:00:00Z',
  updated_at: '2026-09-29T12:00:00Z',
}
const managed = {
  id: 1,
  provider_id: 84,
  domain: 'managed.example.com',
  zone_id: '101',
  auto_manage: true,
  proxied_by_default: false,
  verified: true,
  generated_hostname_mode: 'standard',
  sync_generated_records: false,
  zone_access_ok: true,
  zone_access_error: null,
  verification_error: null,
  created_at: provider.created_at,
  updated_at: provider.updated_at,
}
const zones = {
  zones: [
    {
      id: '101',
      name: 'managed.example.com',
      status: 'active',
      nameservers: [],
    },
    { id: '102', name: 'example.com', status: 'active', nameservers: [] },
  ],
}
async function mock(page: Page) {
  await page.route('**/api/dns-providers/84', (r) =>
    r.fulfill({ json: provider })
  )
  await page.route('**/api/dns-providers/84/domains', (r) =>
    r.fulfill({ json: [managed] })
  )
  await page.route('**/api/dns-providers/84/zones', (r) =>
    r.fulfill({ json: zones })
  )
}
async function wizard(page: Page) {
  await page.goto('/dns-providers/add')
  await page.getByRole('button', { name: /bunny.net DNS/ }).click()
  await page.getByRole('button', { name: 'Next', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill(provider.name)
  await page
    .getByLabel('Description (optional)', { exact: true })
    .fill(provider.description)
  await page.getByRole('button', { name: 'Next', exact: true }).click()
  await expect(page.getByLabel('Bunny API key')).toHaveAttribute(
    'type',
    'password'
  )
  await page.getByLabel('Bunny API key').fill('synthetic-key')
}

test('Bunny DNS creation submits credentials and opens masked details', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  await mock(page)
  let payload: unknown
  await page.route('**/api/dns-providers', async (r) => {
    if (r.request().method() === 'GET') {
      await r.fulfill({ json: [provider] })
      return
    }
    payload = r.request().postDataJSON()
    await r.fulfill({ status: 201, json: provider })
  })
  await wizard(page)
  await page.getByRole('button', { name: 'Create Provider' }).click()
  await expect(page).toHaveURL(/\/dns-providers\/84$/)
  expect(payload).toEqual({
    name: provider.name,
    description: provider.description,
    provider_type: 'bunny',
    credentials: { type: 'bunny', api_key: 'synthetic-key' },
  })
  await expect(page.getByText('********', { exact: true })).toBeVisible()
  await expect(page.getByText('synthetic-key', { exact: true })).toHaveCount(0)
  expect(consoleErrors).toEqual([])
  expect(httpFailures).toEqual([])
})

test('rejected Bunny credentials show the actual error without success', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  await page.route('**/api/dns-providers', (r) =>
    r.request().method() === 'GET'
      ? r.fulfill({ json: [] })
      : r.fulfill({
          status: 400,
          json: { detail: 'Bunny API key rejected while listing DNS zones' },
        })
  )
  await wizard(page)
  await page.getByRole('button', { name: 'Create Provider' }).click()
  await expect(
    page.getByRole('alert').filter({ hasText: 'Bunny API key rejected' })
  ).toBeVisible()
  await expect(page).toHaveURL(/\/dns-providers\/add$/)
  await expect(page.getByText('DNS provider created successfully')).toHaveCount(
    0
  )
  await expect(page.getByLabel('Bunny API key')).toHaveValue('synthetic-key')
  expect(httpFailures).toEqual([
    expect.stringMatching(/400 .*\/api\/dns-providers$/),
  ])
  expect(consoleErrors).toEqual([])
})

test('Bunny zone selection excludes managed zones and submits an account zone', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  await mock(page)
  let payload: unknown
  await page.route('**/api/dns-providers/84/domains', async (r) => {
    if (r.request().method() === 'POST') {
      payload = r.request().postDataJSON()
      await r.fulfill({
        status: 201,
        json: { ...managed, domain: 'example.com', zone_id: '102' },
      })
    } else await r.fulfill({ json: [managed] })
  })
  await page.goto('/dns-providers/84')
  await page.getByRole('button', { name: 'Add zone', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('combobox').click()
  await expect(
    page.getByRole('option', { name: 'managed.example.com', exact: true })
  ).toHaveCount(0)
  await page.getByPlaceholder('Search available zones…').fill('example.com')
  await page.getByRole('option', { name: 'example.com', exact: true }).click()
  await dialog.getByRole('button', { name: 'Add zone', exact: true }).click()
  await expect(page.getByText('Zone added successfully')).toBeVisible()
  expect(payload).toEqual({
    domain: 'example.com',
    auto_manage: true,
    proxied_by_default: false,
    generated_hostname_mode: 'standard',
    sync_generated_records: false,
  })
  expect(consoleErrors).toEqual([])
  expect(httpFailures).toEqual([])
})

test('failed Bunny zone lookup blocks adding a zone and offers retry', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  await mock(page)
  let failed = true
  await page.route('**/api/dns-providers/84/zones', (r) =>
    failed
      ? r.fulfill({
          status: 503,
          json: { detail: 'Bunny DNS zones unavailable' },
        })
      : r.fulfill({ json: zones })
  )
  await page.goto('/dns-providers/84')
  await expect(
    page.getByText('Available zones could not be loaded')
  ).toBeVisible()
  await page.getByRole('button', { name: 'Add zone', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByRole('combobox')).toBeDisabled()
  await expect(
    dialog.getByRole('button', { name: 'Add zone', exact: true })
  ).toBeDisabled()
  await expect(
    dialog.getByText('Bunny DNS zones unavailable', { exact: false })
  ).toBeVisible()
  failed = false
  await dialog.getByRole('button', { name: 'Retry', exact: true }).click()
  await expect(dialog.getByRole('combobox')).toBeEnabled()
  expect(httpFailures.length).toBeGreaterThan(0)
  expect(
    httpFailures.every((f) => /503 .*\/api\/dns-providers\/84\/zones$/.test(f))
  ).toBe(true)
  expect(consoleErrors).toEqual([])
})

test('delivery setup selects Bunny DNS and Bunny CDN independently using the design system', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  const project = {
    id: 84,
    name: 'Bunny delivery demo',
    slug: 'bunny-delivery-demo',
    main_branch: 'main',
    directory: '.',
    source_type: 'docker_image',
    project_type: 'application',
    attack_mode: false,
    deployment_config: {},
    created_at: 0,
    updated_at: 0,
  }
  const profile = {
    id: 91,
    name: 'Bunny CDN demo',
    provider_kind: 'bunny',
    bunny_pull_zone_id: 123,
    bunny_hostname: 'synthetic.b-cdn.net',
    created_at: provider.created_at,
    updated_at: provider.updated_at,
  }
  await mock(page)
  await page.route('**/api/projects', (r) =>
    r.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route('**/api/projects?*', (r) =>
    r.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route('**/api/projects/by-slug/bunny-delivery-demo', (r) =>
    r.fulfill({ json: project })
  )
  await page.route('**/api/projects/84/environments', (r) =>
    r.fulfill({
      json: [{ id: 1, name: 'production', slug: 'production', project_id: 84 }],
    })
  )
  await page.route('**/api/projects/84/custom-domains', (r) =>
    r.fulfill({ json: { domains: [] } })
  )
  await page.route(
    /\/api\/projects\/84\/domain-delivery-bindings(?:\?.*)?$/,
    (r) => fulfillPage(r, [])
  )
  await page.route('**/api/projects/84/delivery-settings', (r) =>
    r.fulfill({
      json: {
        project_id: 84,
        default_profile_id: null,
        effective_default_profile: null,
        environment_overrides: [],
      },
    })
  )
  await page.route(PROFILE_LIST, (r) => fulfillPage(r, [profile]))
  await page.route('**/api/dns-providers', (r) =>
    r.fulfill({ json: [provider] })
  )
  let payload: unknown
  await page.route(
    '**/api/projects/84/domain-delivery-bindings/preview',
    async (r) => {
      payload = r.request().postDataJSON()
      await r.fulfill({
        status: 400,
        json: { detail: 'Synthetic preview stops before any DNS mutation' },
      })
    }
  )
  await page.goto('/projects/bunny-delivery-demo/settings/domains')
  await expect(
    page.getByRole('heading', { name: 'No domains configured yet' })
  ).toBeVisible()
  await expect(
    page.getByRole('button', { name: /^Add domain/ })
  ).toBeVisible()
  const settings = page.getByRole('button', { name: 'DNS and CDN settings' })
  await expect(settings).toHaveAttribute('aria-expanded', 'false')
  await expect(
    page.getByRole('button', { name: 'Configure delivery', exact: true })
  ).toBeHidden()
  await settings.click()
  await expect(settings).toHaveAttribute('aria-expanded', 'true')
  await page
    .getByRole('button', { name: 'Configure delivery', exact: true })
    .click()
  const dialog = page.getByRole('dialog')
  await dialog
    .getByLabel('Hostname', { exact: true })
    .fill('shop.managed.example.com')
  await dialog.getByLabel('Environment', { exact: true }).click()
  await page.getByRole('option', { name: 'production', exact: true }).click()
  await dialog
    .getByLabel('Public origin address', { exact: true })
    .fill('192.0.2.10')
  await dialog.getByLabel('DNS provider', { exact: true }).click()
  await page.getByRole('option', { name: provider.name, exact: true }).click()
  await dialog.getByLabel('Managed zone', { exact: true }).click()
  await page.getByRole('option', { name: managed.domain, exact: true }).click()
  await dialog.getByLabel('Delivery profile', { exact: true }).click()
  await page.getByRole('option', { name: /Bunny CDN demo/ }).click()
  expect(
    await dialog
      .getByRole('combobox')
      .evaluateAll((elements) =>
        elements.every((element) => element.tagName === 'BUTTON')
      )
  ).toBe(true)
  await dialog.getByRole('button', { name: 'Preview setup' }).click()
  await expect(
    dialog.getByText('Synthetic preview stops before any DNS mutation')
  ).toBeVisible()
  expect(payload).toEqual({
    hostname: 'shop.managed.example.com',
    environment_id: 1,
    dns_provider_id: 84,
    zone: managed.domain,
    origin_target: '192.0.2.10',
    delivery_profile_id: 91,
  })
  expect(httpFailures).toEqual([
    expect.stringMatching(/400 .*\/domain-delivery-bindings\/preview$/),
  ])
  expect(consoleErrors).toEqual([])
})

test('Bunny onboarding links open the correct setup and explain DNS separately', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  await page.route(PROFILE_LIST, (r) => fulfillPage(r, []))
  await page.route('**/api/delivery-capabilities', (r) =>
    r.fulfill({
      json: [
        {
          provider_kind: 'direct',
          name: 'Direct origin',
          configured: true,
          requirements: [],
          setup_path: null,
        },
        {
          provider_kind: 'cloudflare',
          name: 'Cloudflare proxy',
          configured: false,
          requirements: [],
          setup_path: '/dns-providers',
        },
        {
          provider_kind: 'bunny',
          name: 'bunny.net CDN',
          configured: false,
          requirements: ['Active Pull Zone and account API key'],
          setup_path: '/delivery-profiles',
        },
      ],
    })
  )
  await page.goto('/delivery-profiles')
  await page
    .getByRole('button', { name: 'Set up bunny.net CDN', exact: true })
    .click()
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByLabel('Pull Zone ID')).toBeVisible()
  await expect(
    dialog.getByText(/a Bunny DNS connection is optional/)
  ).toBeVisible()
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
  await page
    .getByRole('link', { name: 'Connect Bunny DNS', exact: true })
    .click()
  await expect(page).toHaveURL(/provider=bunny/)
  await page.getByRole('button', { name: 'Next', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Onboarding demo')
  await page.getByRole('button', { name: 'Next', exact: true }).click()
  await expect(page.getByLabel('Bunny API key')).toBeVisible()
  await expect(
    page.getByRole('link', { name: 'Open Bunny account settings' })
  ).toHaveAttribute('href', 'https://panel.bunny.net/account')
  await expect(
    page.getByText(/Next, choose an existing DNS zone/)
  ).toBeVisible()
  expect(consoleErrors).toEqual([])
  expect(httpFailures).toEqual([])
})
