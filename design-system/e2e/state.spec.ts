// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'
import { ready, v1 } from './helpers'

/**
 * The functional requirement from `docs/requirements.md`: every page must be
 * rebuildable from its address alone.
 *
 * The test is a **reload signature** — document title, page heading, the
 * active facet, the first three row ids and whatever is currently pressed
 * (range strips and segmented controls). Load, sign, reload, sign again. The
 * two strings are identical or the page has lost the reader's place.
 *
 * A signature rather than a screenshot on purpose: it names the things a
 * reader would notice were gone. A page that comes back on the right record
 * but the wrong facet, with the filter cleared or on page 1 of a list they
 * were on page 2 of, has lost their place whether or not it looks like it.
 */
async function signature(page: Page): Promise<string> {
  return page.evaluate(() => {
    const text = (el: Element | null) => (el?.textContent ?? '').replace(/\s+/g, ' ').trim()
    const heading = text(document.querySelector('h1.op-title') ?? document.querySelector('h1'))
    const facet = text(document.querySelector('[role="tab"][aria-selected="true"]'))
    const rows = Array.from(document.querySelectorAll('[id^="row-"]'))
      .slice(0, 3)
      .map((el) => el.id)
      .join(',')
    // Range strips and Segmented controls both say which value is chosen with
    // `aria-pressed`, so one query covers the range label and the rendering.
    const pressed = Array.from(document.querySelectorAll('[aria-pressed="true"]')).map((el) => text(el)).join('|')
    return [document.title, heading, facet, rows, pressed].join(' § ')
  })
}

/**
 * Console addresses that carry a view. Between them they exercise every key
 * the vocabulary has: tab, filter, sort, page, range, segmented and the Logs
 * query grammar.
 */
const ADDRESSES: ReadonlyArray<{ label: string; path: string }> = [
  { label: 'projects filtered and sorted', path: v1('projects', '&f=acme&sort=-visitors') },
  { label: 'databases sorted by size', path: v1('databases', '&sort=-size') },
  { label: 'project record on the deploys facet', path: v1('api-gateway', '&tab=deploys&env=2') },
  { label: 'project record on the variables facet', path: v1('api-gateway', '&tab=variables&env=matrix') },
  { label: 'deployment record on the checks facet', path: v1('deploy:dep_91a', '&tab=checks&f=health') },
  { label: 'database record on the backups facet', path: v1('db:acme-pg', '&tab=backups&f=daily') },
  { label: 'issue record on the events facet, page 2', path: v1('issue:i_4821', '&tab=events&page=2') },
  { label: 'node record with a 7d window', path: v1('node:hetzner-2', '&range=7d') },
  { label: 'analytics on the pages facet as a flow', path: v1('analytics', '&tab=pages&seg=flow&range=7d') },
  { label: 'traces narrowed to errors', path: v1('traces', '&tab=traces&seg=errors&range=7d') },
  { label: 'uptime filtered', path: v1('uptime', '&f=checkout') },
  { label: 'email on the mail facet, problems only', path: v1('email', '&seg=problems&size=50') },
  { label: 'proxy on the routes facet', path: v1('proxy', '&tab=routes&f=api&range=24h') },
  { label: 'errors over 7d', path: v1('errors', '&seg=all&range=7d') },
  { label: 'settings keys filtered', path: v1('settings:keys', '&f=ci') },
  { label: 'logs, a query and a grouped rendering', path: v1('logs', '&q=level%3Aerror&lv=grouped&range=7d') },
]

for (const { label, path } of ADDRESSES) {
  test(`${label} survives a reload`, async ({ page }) => {
    await page.goto(path)
    await ready(page)
    const before = await signature(page)
    // The address must be worth signing: a signature with no heading means the
    // screen did not render, and would then "match" itself after a reload.
    expect(before.split(' § ')[1], `no heading on ${path}`).not.toBe('')

    await page.reload()
    await ready(page)
    const after = await signature(page)
    expect(after, `${path} did not come back the same`).toBe(before)
  })
}

test('a pasted address opens the same view in a second tab', async ({ page, context }) => {
  const path = v1('issue:i_4821', '&tab=events&page=2')
  await page.goto(path)
  await ready(page)
  const before = await signature(page)

  const href = await page.evaluate(() => window.location.href)
  const second = await context.newPage()
  await second.goto(href)
  await ready(second)
  expect(await signature(second)).toBe(before)
  await second.close()
})

test.describe('the UI writes what it changed into the address', () => {
  test('a facet writes ?tab=', async ({ page }) => {
    await page.goto(v1('api-gateway'))
    await ready(page)
    expect(new URL(page.url()).searchParams.get('tab')).toBeNull()

    await page.getByRole('tab').filter({ has: page.locator('text="deploys"') }).first().click()
    await expect.poll(() => new URL(page.url()).searchParams.get('tab')).toBe('deploys')

    // And back to the default facet removes the key: a default is an absence.
    await page.getByRole('tab').filter({ has: page.locator('text="overview"') }).first().click()
    await expect.poll(() => new URL(page.url()).searchParams.get('tab')).toBeNull()
  })

  test('a filter writes ?f=', async ({ page }) => {
    await page.goto(v1('projects'))
    await ready(page)
    await page.getByRole('textbox', { name: /filter projects/i }).fill('acme')
    await expect.poll(() => new URL(page.url()).searchParams.get('f')).toBe('acme')
  })

  test('a sorted column writes ?sort=', async ({ page }) => {
    await page.goto(v1('databases'))
    await ready(page)
    await page.getByRole('button', { name: 'size', exact: true }).first().click()
    await expect.poll(() => new URL(page.url()).searchParams.get('sort')).toBe('size')
  })

  test('a page writes ?page=', async ({ page }) => {
    await page.goto(v1('issue:i_4821', '&tab=events'))
    await ready(page)
    await page.keyboard.press(']')
    await expect.poll(() => new URL(page.url()).searchParams.get('page')).toBe('2')
  })

  test('a range writes ?range=', async ({ page }) => {
    await page.goto(v1('proxy'))
    await ready(page)
    await page.getByRole('button', { name: '24h', exact: true }).first().click()
    await expect.poll(() => new URL(page.url()).searchParams.get('range')).toBe('24h')
  })
})

test('the logs inspector copies the address you are on', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  await page.goto(v1('logs'))
  await ready(page)

  // Opening a line puts it in the address beside the query, so the panel is
  // part of the view and not a thing only this tab knows about.
  await page.locator('[id^="row-log_"]').first().click()
  await expect.poll(() => new URL(page.url()).searchParams.get('row')).toMatch(/^log_/)

  const inspector = page.getByRole('complementary', { name: /log line inspector/i })
  await expect(inspector).toBeVisible()

  const href = await page.evaluate(() => window.location.href)
  await inspector.getByRole('button', { name: /copy link/i }).click()
  const copied = await page.evaluate(() => navigator.clipboard.readText())
  expect(copied).toBe(href)

  // The panel's `open` is still the record page: `?p=log:<id>` is the deep link.
  const row = new URL(href).searchParams.get('row')
  await inspector.getByRole('button', { name: 'open', exact: true }).click()
  await expect.poll(() => new URL(page.url()).searchParams.get('p')).toBe(`log:${row}`)
})

test('the guide demonstrates the rule it states', async ({ page }) => {
  await page.goto('/guide#requirements')
  await ready(page)
  await expect(page.getByRole('heading', { name: 'The URL is the state' })).toBeVisible()

  const address = page.locator('[data-url-state-demo]')
  await expect(address).toHaveText('/v1?p=deploys')
  await page.getByRole('textbox', { name: 'Filter deployments' }).fill('acme')
  await page.getByRole('button', { name: 'project', exact: true }).click()
  await expect(address).toHaveText('/v1?p=deploys&f=acme&sort=project')

  // The view is thrown away and rebuilt from the string: it has to come back.
  await page.getByRole('button', { name: 'reload' }).click()
  await expect(address).toHaveText('/v1?p=deploys&f=acme&sort=project')
  await expect(page.getByText('rebuilt from the address')).toBeVisible()
})
