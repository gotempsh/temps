// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'
const stamp = '2026-09-09T12:00:00Z'
const traceId = 'a'.repeat(32)
const fixtures = {
  analytics: {
    rows: [
      {
        key: '/checkout',
        project_id: 1,
        project_name: 'Storefront',
        views: 120,
        visitors: 80,
        sessions: 90,
        avg_time_seconds: 30,
      },
    ],
    total: 1,
    total_views: 120,
  },
  traces: {
    data: [
      {
        project_id: 1,
        project_name: 'Storefront',
        project_slug: 'storefront',
        trace_id: traceId,
        root_span_name: 'GET /checkout',
        service_name: 'web',
        duration_ms: 123,
        span_count: 4,
        error_count: 1,
        status_code: 'ERROR',
        start_time: stamp,
        kind: 'SERVER',
      },
    ],
    total: 1,
    projects: [{ id: 1, name: 'Storefront', slug: 'storefront' }],
  },
  errors: {
    data: [
      {
        id: 42,
        title: 'Checkout failed',
        error_type: 'TypeError',
        project_id: 1,
        project_name: 'Storefront',
        project_slug: 'storefront',
        status: 'unresolved',
        events_in_range: 12,
        affected_users: 3,
        last_seen: stamp,
        first_seen: stamp,
        total_count: 50,
      },
    ],
    pagination: { page: 1, page_size: 25, total_count: 1, total_pages: 1 },
  },
  logs: {
    lines: [
      {
        timestamp: stamp,
        level: 'ERROR',
        message: 'Checkout request failed',
        owner: 'Storefront',
        env: 'production',
        service: 'web',
        project_id: 1,
        chunk_id: 'chunk-1',
        line_offset: 1,
      },
    ],
    next_cursor: 'next-token',
    scan_limit_reached: false,
    scanned_bytes: 100,
    scanned_chunks: 1,
  },
}
const endpoints = {
  analytics: '/api/analytics/global',
  traces: '/api/otel/global/trace-summaries',
  errors: '/api/error-groups',
  logs: '/api/logs/global/search',
}
type Kind = keyof typeof fixtures
async function mock(page: Page, kind: Kind) {
  await page.route(/\/api\/projects(?:\?|$)/, (route) =>
    route.fulfill({
      json: {
        projects: [{ id: 1, name: 'Storefront', slug: 'storefront' }],
        total: 1,
      },
    })
  )
  await page.route(
    (url) => url.pathname === endpoints[kind],
    (route) => {
      if (
        kind === 'analytics' &&
        new URL(route.request().url()).searchParams.get('facet') === 'traffic'
      )
        return route.fulfill({
          json: {
            rows: [{ ...fixtures.analytics.rows[0], key: stamp }],
            total: 1,
            total_views: 120,
          },
        })
      return route.fulfill({ json: fixtures[kind] })
    }
  )
}
for (const kind of Object.keys(fixtures) as Kind[]) {
  for (const width of [1440, 390]) {
    test(`${kind} global page renders and preserves scope at ${width}px`, async ({
      page,
    }) => {
      const errors: string[] = []
      page.on('pageerror', (error) => errors.push(error.message))
      await page.setViewportSize({ width, height: 1000 })
      await mock(page, kind)
      await page.goto(`/${kind}`)
      await expect(
        page.getByRole('heading', {
          name: kind[0].toUpperCase() + kind.slice(1),
          exact: true,
        })
      ).toBeVisible()
      await expect(
        kind === 'analytics' && width < 768
          ? page
              .getByRole('list', { name: 'Top pages', exact: true })
              .getByText('Storefront')
          : page.getByRole('cell', { name: 'Storefront', exact: false }).first()
      ).toBeVisible()
      if (kind === 'logs') {
        await page.getByRole('combobox', { name: 'Search log messages' }).fill('project:')
        await page.getByRole('option', { name: /project:Storefront/ }).click()
      } else {
        await page.getByRole('combobox', { name: 'Project scope' }).click()
        await page.getByRole('option', { name: 'Storefront' }).click()
      }
      await expect(page).toHaveURL(/project_id=1/)
      for (const preset of ['1h', '6h', '1d', '7d'])
        await expect(
          page.getByRole('button', { name: preset, exact: true })
        ).toBeVisible()
      await page.getByRole('button', { name: '6h', exact: true }).click()
      await expect(page).toHaveURL(/range=6h/)
      const bounds = new URL(page.url()).searchParams
      expect(
        Date.parse(bounds.get('to')!) - Date.parse(bounds.get('from')!)
      ).toBe(6 * 3600000)
      const frozen = new URL(page.url()).searchParams.get('from')
      await page.reload()
      if (kind === 'logs') await expect(page.getByRole('button', { name: 'Edit project filter' })).toContainText('Storefront')
      else await expect(page.getByRole('combobox', { name: 'Project scope' })).toHaveText('Storefront')
      expect(new URL(page.url()).searchParams.get('from')).toBe(frozen)
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth
        )
      ).toBe(true)
      if (kind === 'traces')
        await expect(
          page.getByRole('link', { name: 'Cross-project waterfall' })
        ).toHaveAttribute('href', `/traces/global/${traceId}`)
      if (kind === 'errors')
        await expect(
          page.getByRole('link', { name: 'Checkout failed' })
        ).toHaveAttribute('href', '/projects/storefront/errors/42')
      expect(errors).toEqual([])
      await page.screenshot({
        path: `/tmp/temps-global-${kind}-${width}.png`,
        fullPage: true,
      })
    })
  }
  test(`${kind} presents access failure and retries`, async ({ page }) => {
    await mock(page, kind)
    let failed = true
    await page.route(
      (url) => url.pathname === endpoints[kind],
      (route) =>
        failed
          ? route.fulfill({ status: 403, json: { title: 'Access denied' } })
          : route.fulfill({ json: fixtures[kind] })
    )
    await page.goto(`/${kind}`)
    await expect(page.getByText('Access denied').first()).toBeVisible()
    failed = false
    await page
      .getByRole('button', { name: `Retry ${kind}`, exact: true })
      .click()
    await expect(
      page.getByRole('cell', { name: 'Storefront', exact: false }).first()
    ).toBeVisible()
  })
}
test('log cursor uses the same time window and is reset by a filter change', async ({
  page,
}) => {
  await mock(page, 'logs')
  await page.goto('/logs')
  await page.getByRole('button', { name: 'Next page', exact: true }).click()
  await expect(page).toHaveURL(/cursor=next-token/)
  const request = page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname === endpoints.logs &&
      req.postDataJSON().text === 'timeout'
  )
  await page
    .getByRole('combobox', { name: 'Search log messages' })
    .fill('timeout')
  expect((await request).postDataJSON().cursor).toBeUndefined()
  await expect(page).not.toHaveURL(/cursor=/)
})
test('scan-budget exhaustion asks for a narrower search instead of claiming no logs', async ({
  page,
}) => {
  await mock(page, 'logs')
  await page.route(
    (url) => url.pathname === endpoints.logs,
    (route) =>
      route.fulfill({
        json: {
          lines: [],
          scan_limit_reached: true,
          scanned_bytes: 100,
          scanned_chunks: 512,
        },
      })
  )
  await page.goto('/logs')
  await expect(
    page.getByText('Search limit reached', { exact: true })
  ).toBeVisible()
  await expect(page.getByText('No logs in this view')).toHaveCount(0)
  await expect(
    page.getByRole('button', { name: 'Next page', exact: true })
  ).toBeDisabled()
})

for (const width of [1440, 390]) {
  test(`custom range validates, applies, and survives reload at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1000 })
    if (width === 1440)
      await page.addInitScript(() => localStorage.setItem('theme', 'dark'))
    await mock(page, 'logs')
    await page.goto('/logs')
    await page.getByRole('button', { name: 'Next page', exact: true }).click()
    await expect(page).toHaveURL(/cursor=next-token/)
    const before = page.url()
    await page
      .getByRole('button', { name: 'Custom time range', exact: true })
      .click()
    await page
      .getByLabel('Start date and time', { exact: true })
      .fill('2026-09-08T09:30')
    await page
      .getByLabel('End date and time', { exact: true })
      .fill('2026-09-07T09:30')
    await page.getByRole('button', { name: 'Apply range' }).click()
    await expect(
      page.getByText('End time must be after start time.')
    ).toBeVisible()
    expect(page.url()).toBe(before)
    await page
      .getByLabel('End date and time', { exact: true })
      .fill('2026-09-09T17:45')
    await page.getByRole('button', { name: 'Apply range' }).click()
    await expect(page).toHaveURL(/range=custom/)
    await expect(page).not.toHaveURL(/cursor=/)
    const custom = new URL(page.url()).searchParams
    const expected = await page.evaluate(() => ({
      from: new Date(2026, 8, 8, 9, 30).toISOString(),
      to: new Date(2026, 8, 9, 17, 45).toISOString(),
    }))
    expect(custom.get('from')).toBe(expected.from)
    expect(custom.get('to')).toBe(expected.to)
    await page.reload()
    await expect(
      page.getByRole('button', { name: 'Custom time range', exact: true })
    ).toHaveAttribute('aria-pressed', 'true')
    await page.getByRole('button', { name: 'Refresh', exact: true }).click()
    expect(new URL(page.url()).searchParams.get('from')).toBe(expected.from)
    expect(new URL(page.url()).searchParams.get('to')).toBe(expected.to)
    await page
      .getByRole('button', { name: 'Custom time range', exact: true })
      .click()
    await expect(
      page.getByLabel('Start date and time', { exact: true })
    ).toHaveValue('2026-09-08T09:30')
    await page
      .getByLabel('Start date and time', { exact: true })
      .fill('2026-09-06T00:00')
    await page.getByRole('button', { name: 'Cancel', exact: true }).click()
    expect(new URL(page.url()).searchParams.get('from')).toBe(expected.from)
    await page
      .getByRole('button', { name: 'Custom time range', exact: true })
      .click()
    await expect(
      page.getByLabel('Start date and time', { exact: true })
    ).toHaveValue('2026-09-08T09:30')
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth
      )
    ).toBe(true)
    await page.screenshot({
      path: `/tmp/temps-custom-range-${width}.png`,
      fullPage: true,
    })
    await page.keyboard.press('Escape')
    await page.getByRole('button', { name: '1d', exact: true }).click()
    await expect(page).toHaveURL(/range=1d/)
    await expect(
      page.getByRole('button', { name: '1d', exact: true })
    ).toHaveAttribute('aria-pressed', 'true')
  })
}

for (const width of [320, 390, 1440]) {
  test(`analytics summaries, ranked pages and presentation tabs at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1000 })
    await mock(page, 'analytics')
    await page.goto('/analytics')
    await expect(page.getByLabel('Analytics summary')).toContainText('Visitors')
    await expect(page.getByLabel('Analytics summary')).toContainText('80')
    if (width < 768) {
      const list = page.getByRole('list', { name: 'Top pages', exact: true })
      await expect(list).toContainText('Storefront')
      await expect(list).toContainText('Avg. time')
      await expect(list).toContainText('30 s')
      const bounds = await list.boundingBox()
      expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width)
    }
    const tabs = page.getByRole('tablist', { name: 'Traffic presentation' })
    await tabs.getByRole('tab', { name: 'Data table' }).click()
    await expect(
      page.getByRole('columnheader', { name: 'Hour (UTC)' })
    ).toBeVisible()
    await tabs.getByRole('tab', { name: 'Data table' }).press('ArrowLeft')
    await expect(
      tabs.getByRole('tab', { name: 'Chart', exact: true })
    ).toHaveAttribute('aria-selected', 'true')
    await expect(page.getByRole('tabpanel')).toBeVisible()
    await page.screenshot({
      path: `/tmp/temps-mobile-analytics-${width}.png`,
      fullPage: true,
    })
  })
}

test('global log explorer inspects records, exports, and applies API facets', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1000 })
  await mock(page, 'logs')
  await page.route(
    (url) => url.pathname === endpoints.logs,
    (route) =>
      route.fulfill({
        json: {
          ...fixtures.logs,
          lines: [
            {
              ...fixtures.logs.lines[0],
              node_id: 7,
              node_name: 'worker-7',
              fields: { request_id: 'request-test', duration_ms: 250 },
            },
          ],
        },
      })
  )
  await page.goto('/logs')
  await expect(
    page.getByRole('complementary', { name: 'Log facets' })
  ).toContainText('Counts from this page only')
  const row = page.getByRole('button', {
    name: 'Inspect log: Checkout request failed',
  })
  await row.click()
  const record = page.getByRole('complementary', { name: 'Log record' })
  await expect(
    record.getByRole('heading', { name: 'Log record' })
  ).toBeFocused()
  await expect(record).toContainText('request-test')
  await expect(record).toContainText('worker-7')
  await record.getByRole('button', { name: 'Close log record' }).click()
  await expect(row).toBeFocused()
  await page.getByRole('button', { name: 'Wrap', exact: true }).click()
  await expect(
    page.getByRole('button', { name: 'Wrap', exact: true })
  ).toHaveAttribute('aria-pressed', 'true')
  const download = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Export page', exact: true }).click()
  expect((await download).suggestedFilename()).toBe('logs-current-page.ndjson')
  const request = page.waitForRequest(
    (request) =>
      request.url().includes(endpoints.logs) &&
      request.postDataJSON()?.node_ids?.[0] === 7
  )
  await page
    .getByRole('region', { name: 'Node facets', exact: true })
    .getByRole('button')
    .click()
  await request
  await expect(page).toHaveURL(/node_id=7/)
  await expect(page).not.toHaveURL(/cursor=/)
  await page.reload()
  await expect(
    page.getByRole('button', { name: 'Clear node: 7' })
  ).toBeVisible()
  await page.screenshot({
    path: '/tmp/temps-global-log-explorer-desktop.png',
    fullPage: true,
  })
  const clearRequest = page.waitForRequest(
    (request) =>
      request.url().includes(endpoints.logs) &&
      request.postDataJSON()?.node_ids?.length === 0
  )
  await page.getByRole('button', { name: 'Clear node: 7' }).click()
  await clearRequest
})


test('logs reference workspace supports grouping, columns, facets and chart range selection', async ({ page }) => {
  await page.setViewportSize({width: 1440, height: 1000})
  await mock(page, 'logs')
  await page.route((url) => url.pathname === endpoints.logs, route => route.fulfill({json: {...fixtures.logs, lines: Array.from({length: 60}, (_, i) => ({...fixtures.logs.lines[0], line_offset: i, timestamp: new Date(Date.parse(stamp) - i * 30000).toISOString(), level: i % 7 === 0 ? 'ERROR' : i % 3 === 0 ? 'WARN' : 'INFO', message: i % 7 === 0 ? 'Checkout request failed' : `Completed GET /products in ${20 + i}ms`, deploy_id: 42, node_id: 7, node_name: 'worker-7'}))}}))
  await page.goto('/logs?range=custom&from=2026-09-09T11:00:00Z&to=2026-09-09T13:00:00Z')
  await expect(page.getByText('Volume by level', {exact:true})).toBeVisible()
  await expect(page.getByRole('columnheader', {name:'deployment', exact:true})).toBeVisible()
  await page.screenshot({path:'/tmp/temps-logs-reference-desktop.png', fullPage:true})
  await page.getByRole('button', {name:'Next page',exact:true}).click()
  await expect(page).toHaveURL(/cursor=next-token/)
  await page.getByRole('button', {name:'Patterns',exact:true}).click()
  await expect(page.getByText('Exact repeated messages on this loaded page.', {exact:false})).toBeVisible()
  await expect(page).toHaveURL(/cursor=next-token/)
  await page.getByRole('button', {name:'By service',exact:true}).click()
  await expect(page.getByRole('button', {name:'Storefront / web',exact:true})).toBeVisible()
  await page.getByRole('button', {name:'List',exact:true}).click()
  await page.getByRole('button', {name:'Columns',exact:true}).click()
  await page.getByRole('menuitemcheckbox', {name:'node',exact:true}).click()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('columnheader', {name:'node',exact:true})).toBeVisible()
  await page.getByRole('textbox', {name:'Filter facets'}).fill('worker')
  await expect(page.getByRole('region', {name:'Node facets'})).toBeVisible()
  await expect(page.getByRole('region', {name:'Level facets'})).toHaveCount(0)
  await page.getByRole('button', {name:'Show volume table'}).click()
  await page.getByRole('button', {name:/Select time bucket/}).first().click()
  await expect(page).not.toHaveURL(/cursor=/)
  const params = new URL(page.url()).searchParams
  expect(Date.parse(params.get('to')!) - Date.parse(params.get('from')!)).toBe(60000)
})

test('log key:value autocomplete applies, edits and validates filters', async ({ page }) => {
  await page.setViewportSize({width:1440, height:1000})
  await mock(page, 'logs')
  await page.goto('/logs')
  const input = page.getByRole('combobox', {name:'Search log messages'})
  await expect(page.getByRole('combobox', {name:'Project scope'})).toHaveCount(0)
  await expect(page.getByRole('combobox', {name:'Log level'})).toHaveCount(0)
  await input.fill('lev')
  await input.press('Enter')
  await expect(input).toHaveValue('level:')
  await input.press('Enter')
  await expect(page.getByRole('button', {name:'Edit level filter'})).toHaveText('level:error')
  await page.getByRole('button', {name:'Next page',exact:true}).click()
  const request = page.waitForRequest(req => new URL(req.url()).pathname === endpoints.logs && req.postDataJSON().levels?.[0] === 'WARN' && req.postDataJSON().envs?.[0] === 'Production')
  await input.fill('level:warn env:Production timeout')
  await input.press('Enter')
  const body = (await request).postDataJSON()
  expect(body.text).toBe('timeout')
  expect(body.cursor).toBeUndefined()
  await expect(page).not.toHaveURL(/cursor=/)
  await page.reload()
  await expect(page.getByRole('button', {name:'Edit env filter'})).toHaveText('env:Production')
  await page.getByRole('button', {name:'Edit level filter'}).click()
  await input.fill('level:info')
  await input.press('Enter')
  await expect(page.getByRole('button', {name:'Edit level filter'})).toHaveText('level:info')
  await input.fill('node:invalid')
  await input.press('Enter')
  await expect(page.getByRole('alert')).toContainText('positive numeric ID')
  await expect(page).not.toHaveURL(/node_id=/)
  await input.press('Escape')
  await expect(input).toHaveAttribute('aria-expanded','false')
  await input.fill('project:store')
  await page.getByRole('option', {name:/project:Storefront/}).click()
  await expect(page).toHaveURL(/project_id=1/)
  await input.fill('source:service')
  await input.press('Enter')
  await expect(page).toHaveURL(/source=service/)
  await expect(page).not.toHaveURL(/project_id=/)
  await page.getByRole('button', {name:'Clear source: service'}).click()
  await expect(page).not.toHaveURL(/source=/)
  await input.fill('level:')
  await page.screenshot({path:'/tmp/temps-logs-key-value-autocomplete.png',fullPage:true})
})


test('log autocomplete remains usable on a narrow screen', async ({ page }) => {
  await page.setViewportSize({width:390, height:844})
  await mock(page, 'logs')
  await page.goto('/logs')
  const input = page.getByRole('combobox', {name:'Search log messages'})
  await input.fill('env:')
  await page.getByRole('option', {name:'env:production', exact:true}).click()
  await expect(page.getByRole('button', {name:'Edit env filter'})).toHaveText('env:production')
  await input.fill('level:')
  await input.press('ArrowDown')
  await input.press('Tab')
  await expect(page.getByRole('button', {name:'Edit level filter'})).toHaveText('level:warn')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})
