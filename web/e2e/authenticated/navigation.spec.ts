// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, expectAppMounted, test } from '../fixtures'

/**
 * Walks the platform navigation as a signed-in user.
 *
 * Routes are read out of the rendered sidebar rather than hardcoded here, for
 * two reasons: the list stays correct as the nav changes, and a nav entry
 * pointing at a route that does not exist becomes a test failure. That is a
 * real bug class -- while building this suite, a "Databases" entry was found
 * to link to /storage, and visiting /databases directly renders "404 - Page
 * Not Found" inside a perfectly healthy app shell.
 *
 * The sidebar lists projects and one Server row; the server's own pages
 * (nodes, databases, domains) are the tabs of the Server page, each a menu
 * of its pages, so their links are read from those menus.
 *
 * Note that "the page rendered" is NOT sufficient as an assertion: the 404 page
 * mounts fine and gives #root children. Hence the explicit not-404 check.
 */
test.describe('platform navigation', () => {
  test('every sidebar destination renders real content', async ({
    page,
    consoleErrors,
  }) => {
    await page.goto('/projects')
    await expectAppMounted(page)

    // #root gains children as soon as the shell paints, which is before the
    // sidebar has rendered -- collecting links at that point silently finds
    // zero and the spec would "pass" by testing nothing. Wait for a known
    // destination to exist first.
    const serverRow = page.locator('[data-branch="server"] a').first()
    await expect(serverRow).toBeAttached({ timeout: 30_000 })

    // The sidebar is not marked up as <nav>/<aside>, so scoping by landmark
    // finds nothing. Collect internal links and filter instead.
    const internalLinks = () =>
      page.locator('a[href^="/"]').evaluateAll((nodes) =>
        nodes
          .map((node) => node.getAttribute('href') ?? '')
          .filter(
            (href) =>
              href.startsWith('/') &&
              // Project-scoped pages need a project to exist; covered by the
              // project-creation spec instead.
              !href.startsWith('/projects/') &&
              // Query strings are pre-filtered entry points (import wizards),
              // not distinct destinations.
              !href.includes('?') &&
              href !== '/'
          )
      )
    const found = new Set(await internalLinks())

    // The Server page: each of its tabs opens a menu of its pages.
    await serverRow.click()
    const serverViews = page.getByRole('navigation', {
      name: 'Server views',
      exact: true,
    })
    await expect(serverViews).toBeVisible()
    const tabs = serverViews.locator('[data-tab]')
    for (let index = 0; index < (await tabs.count()); index++) {
      await tabs.nth(index).click()
      const menu = page.getByRole('menu')
      await expect(menu).toBeVisible()
      for (const href of await menu
        .locator('a[href^="/"]')
        .evaluateAll((nodes) =>
          nodes.map((node) => node.getAttribute('href') ?? '')
        ))
        found.add(href)
      await page.keyboard.press('Escape')
      await expect(menu).toBeHidden()
    }
    for (const path of ['/storage', '/domains', '/settings/nodes'])
      expect(found, `the Server page should reach ${path}`).toContain(path)
    const hrefs = [...found]

    expect(
      hrefs.length,
      'expected to discover sidebar destinations'
    ).toBeGreaterThan(3)

    const broken: string[] = []
    for (const href of hrefs) {
      await page.goto(href)
      await expectAppMounted(page)

      if (
        await page
          .getByText(/404\s*-\s*Page Not Found/i)
          .isVisible()
          .catch(() => false)
      ) {
        broken.push(href)
      }
    }

    expect(broken, 'sidebar links that render a 404 page').toEqual([])
    expect(consoleErrors).toEqual([])
  })

  test('the databases page offers provisionable engines', async ({
    page,
    consoleErrors,
  }) => {
    await page.goto('/storage')
    await expectAppMounted(page)

    await expect(
      page.getByRole('heading', { name: 'Databases', exact: true })
    ).toBeVisible()

    // The provider catalogue is the entry point for every managed-service flow,
    // so an empty list here means database provisioning is unreachable. A
    // fresh instance shows it inline; once a database exists it moves behind
    // the header's "Create database" menu, which a user opens to see it.
    const offered = (engine: string) =>
      page.getByText(engine, { exact: true }).filter({ visible: true }).first()
    const createMenu = page.getByRole('button', { name: /^Create database/ })
    await expect(createMenu.or(offered('PostgreSQL')).first()).toBeVisible()
    if (await createMenu.isVisible()) {
      await createMenu.click()
    }
    for (const engine of ['PostgreSQL', 'Redis', 'MongoDB', 'MariaDB']) {
      await expect(
        offered(engine),
        `${engine} should be offered on the databases page`
      ).toBeVisible()
    }

    expect(consoleErrors).toEqual([])
  })
})
