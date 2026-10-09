// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AcmeOrderResponse,
  DnsProviderResponse,
  DomainResponse,
  EmailDomainResponse,
  EmailProviderResponse,
  ListRenewalAttemptsResponse,
} from '../../src/api/client'
import { expect, test } from '../fixtures'

const timestamp = '2026-01-01T00:00:00Z'

for (const failedEndpoint of ['email-providers', 'email-domains'] as const) {
  test(`Retry email data recovers from a failed ${failedEndpoint} request`, async ({
    page,
    consoleErrors,
    httpFailures,
  }) => {
    const provider: EmailProviderResponse = {
      id: 41,
      name: 'Configured email',
      provider_type: 'smtp',
      credentials: {},
      region: 'local',
      is_active: true,
      created_at: timestamp,
      updated_at: timestamp,
    }
    const domain: EmailDomainResponse = {
      id: 42,
      provider_id: provider.id,
      domain: 'mail.example.test',
      status: 'verified',
      created_at: timestamp,
      updated_at: timestamp,
    }
    const requests = { 'email-providers': 0, 'email-domains': 0 }
    let recover = false
    for (const endpoint of ['email-providers', 'email-domains'] as const) {
      await page.route(
        new RegExp(`/api/${endpoint}(?:\\?.*)?$`),
        async (route) => {
          requests[endpoint] += 1
          // Keep automatic query retries failed until the user presses Retry.
          if (endpoint === failedEndpoint && !recover) {
            await route.fulfill({
              status: 503,
              json: {
                title: 'Service Unavailable',
                detail: 'Temporary lookup failure',
              },
            })
            return
          }
          await route.fulfill({
            json: endpoint === 'email-providers' ? [provider] : [domain],
          })
        }
      )
    }

    await page.goto('/email?tab=domains')
    await expect(
      page.getByRole('heading', { name: 'Email', exact: true })
    ).toBeVisible({ timeout: 60_000 })
    const retry = page.getByRole('button', {
      name: 'Retry email data',
      exact: true,
    })
    await expect(
      page.getByText('Email data unavailable', { exact: true })
    ).toBeVisible({ timeout: 30_000 })
    await expect(retry).toBeEnabled()
    await expect(page.getByText(domain.domain, { exact: true })).toHaveCount(0)
    const beforeRetry = { ...requests }
    expect(beforeRetry[failedEndpoint]).toBeGreaterThan(0)

    recover = true
    await retry.click()

    await expect(page.getByText(domain.domain, { exact: true })).toBeVisible()
    await expect(page.getByText('smtp', { exact: true })).toBeVisible()
    await expect(
      page.getByText('Email data unavailable', { exact: true })
    ).toHaveCount(0)
    await expect(retry).toHaveCount(0)
    // The shared button must refetch both lookups, including the healthy one.
    await expect
      .poll(() => requests['email-providers'])
      .toBeGreaterThan(beforeRetry['email-providers'])
    await expect
      .poll(() => requests['email-domains'])
      .toBeGreaterThan(beforeRetry['email-domains'])
    expect(
      httpFailures.filter(
        (failure) =>
          !failure.startsWith('503 ') ||
          !failure.includes(`/api/${failedEndpoint}`)
      )
    ).toEqual([])
    expect(consoleErrors).toEqual([])
  })
}

test('Retry DNS providers restores the configured provider and clears the lookup error', async ({
  page,
  consoleErrors,
  httpFailures,
}) => {
  const domain: DomainResponse = {
    id: 42,
    domain: 'app.example.test',
    status: 'pending',
    verification_method: 'dns-01',
    is_wildcard: false,
    created_at: Date.parse(timestamp),
    updated_at: Date.parse(timestamp),
  }
  const order: AcmeOrderResponse = {
    id: 43,
    domain_id: domain.id,
    status: 'pending',
    identifiers: [],
    authorizations: {
      challenge_type: 'dns-01',
      dns_txt_records: [
        { name: '_acme-challenge.app.example.test', value: 'challenge-value' },
      ],
    },
    order_url: 'https://acme.example.test/order/43',
    email: 'operator@example.test',
    created_at: domain.created_at,
    updated_at: domain.updated_at,
  }
  const provider: DnsProviderResponse = {
    id: 44,
    name: 'Configured DNS',
    provider_type: 'cloudflare',
    credentials: {},
    flat_hostnames_supported: true,
    is_active: true,
    created_at: timestamp,
    updated_at: timestamp,
  }
  await page.route(/\/api\/domains\/42$/, (route) =>
    route.fulfill({ json: domain })
  )
  await page.route(/\/api\/domains\/42\/order$/, (route) =>
    route.fulfill({ json: order })
  )
  const renewalHistory: ListRenewalAttemptsResponse = {
    attempts: [],
    page: 1,
    page_size: 10,
    total: 0,
  }
  await page.route(
    /\/api\/domains\/app\.example\.test\/renewal-attempts(?:\?.*)?$/,
    (route) => route.fulfill({ json: renewalHistory })
  )
  const assignmentPath = '/api/projects/custom-domains/by-host/app.example.test'
  await page.route(
    /\/api\/projects\/custom-domains\/by-host\/app\.example\.test$/,
    (route) =>
      route.fulfill({
        status: 404,
        json: {
          title: 'Not Found',
          detail: 'The fixture domain has no project assignment',
        },
      })
  )
  let requests = 0
  let recover = false
  await page.route(/\/api\/dns-providers(?:\?.*)?$/, async (route) => {
    requests += 1
    await route.fulfill(
      recover
        ? { json: [provider] }
        : {
            status: 503,
            json: {
              title: 'Service Unavailable',
              detail: 'Temporary provider lookup failure',
            },
          }
    )
  })

  await page.goto('/domains/42')
  await expect(
    page.getByRole('heading', { name: domain.domain, exact: true })
  ).toBeVisible({ timeout: 60_000 })
  const retry = page.getByRole('button', {
    name: 'Retry DNS providers',
    exact: true,
  })
  await expect(
    page.getByRole('alert').filter({ hasText: 'Could not load DNS providers' })
  ).toBeVisible({ timeout: 30_000 })
  await expect(retry).toHaveAttribute('aria-disabled', 'false')
  await expect(
    page.getByText('Value: challenge-value', { exact: true })
  ).toBeVisible()
  const beforeRetry = requests
  expect(beforeRetry).toBeGreaterThan(0)

  recover = true
  await retry.click()

  const select = page
    .getByRole('combobox')
    .filter({ hasText: 'Select provider' })
  await expect(select).toBeVisible()
  await expect(retry).toHaveCount(0)
  await expect(
    page.getByRole('alert').filter({ hasText: 'Could not load DNS providers' })
  ).toHaveCount(0)
  await select.click()
  await page
    .getByRole('option', { name: 'Configured DNS (cloudflare)', exact: true })
    .click()
  await expect(page.getByRole('combobox')).toContainText('Configured DNS')
  await expect(
    page.getByRole('button', { name: 'Auto-create', exact: true })
  ).toBeVisible()
  await expect.poll(() => requests).toBeGreaterThan(beforeRetry)
  expect(
    httpFailures.filter(
      (failure) =>
        !(
          failure.startsWith('503 ') &&
          /\/api\/dns-providers(?:\?.*)?$/.test(failure)
        ) && !(failure.startsWith('404 ') && failure.endsWith(assignmentPath))
    )
  ).toEqual([])
  expect(consoleErrors).toEqual([])
})
