// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'
import {
  PROJECT_GROUPS_LIST_API,
  PROJECTS_LIST_API,
  groupFixture,
  projectGroupsListBody,
  serviceFixture,
  servicesListBody,
  type GroupFixture,
  type ServiceFixture,
} from '../fixtures'

/**
 * The projects list after the rename: a "Project" groups services, a
 * "Service" is what used to be called a project. Covers the one-time notice
 * that explains the rename, and which empty state an install with no
 * services gets. Route-mocked, so it never touches real backend state.
 */

const RENAME_NOTICE = 'Your apps are now called Services.'
// The first-run empty state is a labelled landmark rather than a headline.
const ONBOARDING_REGION = 'Get started with services'
const RENAME_NOTICE_KEY = 'temps.servicesRenameNoticeDismissed'

/**
 * Mocks the two list endpoints `/projects` reads and records every services
 * request, so a spec can assert which list the page asked for.
 */
async function mockList(
  page: Page,
  { services, groups }: { services: ServiceFixture[]; groups: GroupFixture[] }
) {
  const serviceRequests: URL[] = []
  await page.route(PROJECT_GROUPS_LIST_API, (route) =>
    route.fulfill({ json: projectGroupsListBody(groups) })
  )
  await page.route(PROJECTS_LIST_API, (route) => {
    const url = new URL(route.request().url())
    serviceRequests.push(url)
    return route.fulfill({ json: servicesListBody(url, services, groups) })
  })
  // Onboarding waits on the Git providers to pick its first step; answer
  // with none so the empty states don't depend on the instance's setup.
  await page.route(/\/api\/git-providers(?:\?|$)/, (route) =>
    route.fulfill({ json: [] })
  )
  return serviceRequests
}

function collectPageErrors(page: Page): string[] {
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  return errors
}

test.describe('services rename notice', () => {
  test('shows with a service and stays dismissed across reloads', async ({
    page,
  }) => {
    const errors = collectPageErrors(page)
    await mockList(page, {
      services: [serviceFixture(910001, 'Billing API')],
      groups: [],
    })
    await page.goto('/projects')
    await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
    await expect(page.getByText(RENAME_NOTICE)).toBeVisible()
    // The notice points at what replaces the old meaning of "project".
    await expect(
      page.getByRole('button', { name: 'Create a project' }).first()
    ).toBeVisible()

    await page.getByRole('button', { name: 'Dismiss this notice' }).click()
    await expect(page.getByText(RENAME_NOTICE)).toHaveCount(0)
    expect(
      await page.evaluate(
        (key) => window.localStorage.getItem(key),
        RENAME_NOTICE_KEY
      )
    ).toBe('1')

    await page.reload()
    await expect(page.getByRole('link', { name: /Billing API/ })).toBeVisible()
    await expect(page.getByText(RENAME_NOTICE)).toHaveCount(0)
    expect(errors).toEqual([])
  })

  test('does not show on an install with no services', async ({ page }) => {
    const errors = collectPageErrors(page)
    await mockList(page, { services: [], groups: [] })
    await page.goto('/projects')
    await expect(
      page.getByRole('region', { name: ONBOARDING_REGION, exact: true })
    ).toBeVisible()
    await expect(page.getByText(RENAME_NOTICE)).toHaveCount(0)
    expect(errors).toEqual([])
  })
})

test.describe('empty states', () => {
  test('an empty install gets first-run onboarding', async ({ page }) => {
    const errors = collectPageErrors(page)
    const serviceRequests = await mockList(page, { services: [], groups: [] })
    await page.goto('/projects')
    await expect(
      page.getByRole('region', { name: ONBOARDING_REGION, exact: true })
    ).toBeVisible()
    // Nothing to filter yet.
    await expect(
      page.getByRole('textbox', {
        name: 'Filter projects and services by name or slug',
      })
    ).toHaveCount(0)
    // Without groups every service is listed, so the page never narrows the
    // request to ungrouped services.
    expect(serviceRequests.length).toBeGreaterThan(0)
    expect(
      serviceRequests.every((url) => !url.searchParams.has('ungrouped'))
    ).toBe(true)
    expect(errors).toEqual([])
  })

  test('an empty install still gets onboarding when projects cannot be loaded', async ({
    page,
  }) => {
    const errors = collectPageErrors(page)
    await mockList(page, { services: [], groups: [] })
    // Registered after the mock above, so it answers the groups request.
    await page.route(PROJECT_GROUPS_LIST_API, (route) =>
      route.fulfill({
        status: 403,
        contentType: 'application/problem+json',
        json: {
          type: 'about:blank',
          title: 'Forbidden',
          status: 403,
          detail: 'Missing permission to list projects',
        },
      })
    )
    await page.goto('/projects')
    await expect(
      page.getByRole('region', { name: ONBOARDING_REGION, exact: true })
    ).toBeVisible()
    // The failure is still said, above the onboarding.
    await expect(
      page
        .getByRole('alert')
        .filter({ hasText: 'Projects could not be loaded' })
    ).toBeVisible()
    expect(errors).toEqual([])
  })

  test('a project with no services shows its card instead of onboarding', async ({
    page,
  }) => {
    const errors = collectPageErrors(page)
    const serviceRequests = await mockList(page, {
      services: [],
      groups: [groupFixture(920001, 'Checkout Suite')],
    })
    await page.goto('/projects')
    const main = page.getByRole('main')
    await expect(
      main.getByRole('link', { name: 'Checkout Suite', exact: true })
    ).toHaveAttribute('href', '/p/checkout-suite')
    // The only card on the page, so its empty state is addressed directly.
    await expect(
      main.getByText('No services yet', { exact: true })
    ).toBeVisible()
    await expect(
      main.getByRole('button', { name: 'Add services', exact: true })
    ).toBeVisible()
    await expect(
      page.getByRole('region', { name: ONBOARDING_REGION, exact: true })
    ).toHaveCount(0)
    // No services, so nothing was renamed for this user.
    await expect(page.getByText(RENAME_NOTICE)).toHaveCount(0)
    // With a group present, only the services outside it are asked for.
    await expect
      .poll(() =>
        serviceRequests.some(
          (url) => url.searchParams.get('ungrouped') === 'true'
        )
      )
      .toBe(true)
    expect(errors).toEqual([])
  })

  test('grouped services show inside their project card, not on their own', async ({
    page,
  }) => {
    const errors = collectPageErrors(page)
    const storefront = serviceFixture(930001, 'Storefront')
    const billing = serviceFixture(930002, 'Billing API')
    const docs = serviceFixture(930003, 'Documentation')
    await mockList(page, {
      services: [storefront, billing, docs],
      groups: [groupFixture(930100, 'Commerce', [storefront, billing])],
    })
    await page.goto('/projects')
    const main = page.getByRole('main')
    await expect(
      main.getByRole('link', { name: 'Commerce', exact: true })
    ).toHaveAttribute('href', '/p/commerce')
    await expect(
      main.getByRole('link', { name: /Documentation/ })
    ).toBeVisible()
    // Each grouped service appears once: as a row of the Commerce card, not
    // also as a card of its own.
    await expect(main.locator('a[href="/projects/storefront"]')).toHaveCount(1)
    await expect(main.locator('a[href="/projects/billing-api"]')).toHaveCount(1)
    await expect(page.getByText(RENAME_NOTICE)).toBeVisible()
    expect(errors).toEqual([])
  })
})
