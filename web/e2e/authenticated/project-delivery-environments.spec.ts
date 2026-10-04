// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page, type Route } from '@playwright/test'

/**
 * Answer a paginated list request with one page holding `items`, out of
 * `total` rows overall.
 */
function fulfillPage(route: Route, items: unknown[], total = items.length) {
  const query = new URL(route.request().url()).searchParams
  return route.fulfill({
    json: {
      items,
      total,
      page: Number(query.get('page') ?? 1),
      page_size: Number(query.get('page_size') ?? 20),
    },
  })
}

const project = {
  id: 347,
  name: 'Delivery settings demo',
  slug: 'delivery-settings-demo',
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
  id: 41,
  name: 'Cloudflare delivery',
  provider_kind: 'cloudflare',
  bunny_pull_zone_id: null,
  bunny_hostname: null,
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
}

const bunnyProfile = {
  ...profile,
  id: 42,
  name: 'Bunny delivery',
  provider_kind: 'bunny',
  bunny_pull_zone_id: 12,
  bunny_hostname: 'delivery.b-cdn.net',
}

type UpdatePayload = {
  default_profile_id: number | null
  environment_overrides: Array<{
    environment_id: number
    profile_id: number | null
  }>
}

async function mockDelivery(
  page: Page,
  environmentCount: number,
  onUpdate?: (payload: UpdatePayload) => void,
  profiles: unknown[] = [profile, bunnyProfile],
  profileTotal = profiles.length
) {
  await page.route('**/api/projects?*', (route) =>
    route.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route('**/api/projects', (route) =>
    route.fulfill({ json: { projects: [project], total: 1 } })
  )
  await page.route(`**/api/projects/by-slug/${project.slug}`, (route) =>
    route.fulfill({ json: project })
  )
  await page.route(`**/api/projects/${project.id}/environments`, (route) =>
    route.fulfill({
      json: Array.from({ length: environmentCount }, (_, index) => ({
        id: index + 1,
        name: index === 0 ? 'production' : 'staging',
        slug: index === 0 ? 'production' : 'staging',
        project_id: project.id,
      })),
    })
  )
  await page.route(/\/api\/delivery-profiles(?:\?.*)?$/, (route) =>
    fulfillPage(route, profiles, profileTotal)
  )
  await page.route(
    `**/api/projects/${project.id}/delivery-settings`,
    (route) => {
      if (route.request().method() === 'PUT') {
        onUpdate?.(route.request().postDataJSON() as UpdatePayload)
      }
      return route.fulfill({
        json: {
          project_id: project.id,
          default_profile_id: profile.id,
          effective_default_profile: profile,
          environment_overrides: Array.from(
            { length: environmentCount },
            (_, index) => ({
              environment_id: index + 1,
              profile_id: index === 0 ? profile.id : null,
            })
          ),
        },
      })
    }
  )
  await page.route(
    new RegExp(
      `/api/projects/${project.id}/domain-delivery-bindings(?:\\?.*)?$`
    ),
    (route) => fulfillPage(route, [])
  )
  await page.route(`**/api/projects/${project.id}/custom-domains*`, (route) =>
    route.fulfill({ json: { domains: [], total: 0 } })
  )
}

async function openDeliverySettings(page: Page) {
  await page.goto(`/projects/${project.slug}/settings/domains`)
  // Delivery lives in the collapsed "DNS and CDN settings" section, below
  // the project's domains.
  await page.getByRole('button', { name: 'DNS and CDN settings' }).click()
}

test('one environment inherits the project default without an override control', async ({
  page,
}) => {
  await mockDelivery(page, 1)
  await openDeliverySettings(page)

  await expect(page.getByText('Project default', { exact: true })).toBeVisible()
  await expect(
    page.getByRole('link', { name: 'Manage DNS providers' })
  ).toHaveAttribute('href', '/dns-providers')
  await expect(
    page.getByText('Environment overrides', { exact: true })
  ).toHaveCount(0)
  await expect(page.getByText('production', { exact: true })).toHaveCount(0)
})

test('multiple environments have a separate override section', async ({
  page,
}) => {
  await mockDelivery(page, 2)
  await openDeliverySettings(page)

  await expect(page.getByText('Project default', { exact: true })).toBeVisible()
  const overrides = page.getByRole('button', { name: /Environment overrides/ })
  await expect(overrides).toHaveAttribute('aria-expanded', 'false')
  await expect(overrides).toContainText('1 configured')
  await expect(page.getByText('production', { exact: true })).toHaveCount(0)
  await overrides.click()
  await expect(overrides).toHaveAttribute('aria-expanded', 'true')
  await expect(page.getByText('production', { exact: true })).toBeVisible()
  await expect(page.getByText('staging', { exact: true })).toBeVisible()
  await overrides.click()
  await expect(page.getByText('production', { exact: true })).toHaveCount(0)
})

test('changing the project provider preserves distinct environment overrides', async ({
  page,
}) => {
  let update: UpdatePayload | undefined
  await mockDelivery(page, 2, (payload) => {
    update = payload
  })
  await openDeliverySettings(page)
  await page
    .getByRole('group', { name: 'Delivery provider' })
    .getByRole('button', { name: /bunny.net/ })
    .click()

  await expect
    .poll(() => update)
    .toEqual({
      default_profile_id: bunnyProfile.id,
      environment_overrides: [
        { environment_id: 1, profile_id: profile.id },
        { environment_id: 2, profile_id: null },
      ],
    })
})

test('changing the provider clears a hidden single-environment override', async ({
  page,
}) => {
  let update: UpdatePayload | undefined
  await mockDelivery(page, 1, (payload) => {
    update = payload
  })
  await openDeliverySettings(page)
  await page
    .getByRole('group', { name: 'Delivery provider' })
    .getByRole('button', { name: /bunny.net/ })
    .click()

  await expect
    .poll(() => update)
    .toEqual({
      default_profile_id: bunnyProfile.id,
      environment_overrides: [{ environment_id: 1, profile_id: null }],
    })
})

test('choosing a provider with several profiles asks which one instead of guessing', async ({
  page,
}) => {
  let update: UpdatePayload | undefined
  const secondBunny = { ...bunnyProfile, id: 43, name: 'Bunny EU delivery' }
  await mockDelivery(
    page,
    1,
    (payload) => {
      update = payload
    },
    [profile, bunnyProfile, secondBunny]
  )
  await openDeliverySettings(page)
  await page
    .getByRole('group', { name: 'Delivery provider' })
    .getByRole('button', { name: /bunny.net/ })
    .click()

  await expect(
    page.getByText(
      'You have 2 Bunny profiles. Choose one in Project default, then save.'
    )
  ).toBeVisible()
  expect(update).toBeUndefined()
})

test('a partial profile list says so and never picks a provider profile for you', async ({
  page,
}) => {
  let update: UpdatePayload | undefined
  // The picker asks for the first 100 profiles by name; this instance has
  // 150, and only one Bunny profile is among those listed.
  await mockDelivery(
    page,
    1,
    (payload) => {
      update = payload
    },
    [profile, bunnyProfile],
    150
  )
  await openDeliverySettings(page)

  await expect(
    page.getByText(
      'The picker lists the first 2 of 150 delivery profiles by name; search in it to find the others.'
    )
  ).toBeVisible()
  await page
    .getByRole('group', { name: 'Delivery provider' })
    .getByRole('button', { name: /bunny.net/ })
    .click()
  await expect(
    page.getByText(
      'Not every profile is listed here, so there may be several Bunny profiles. Choose one in Project default, then save.'
    )
  ).toBeVisible()
  expect(update).toBeUndefined()
})
