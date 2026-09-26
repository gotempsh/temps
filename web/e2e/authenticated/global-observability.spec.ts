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
        stream: 'stdout',
        message: 'Checkout request failed',
        owner: 'Storefront',
        env: 'production',
        service: 'web',
        project_id: 1,
        container_id: 'container-1',
        // Decimal string, past Number.MAX_SAFE_INTEGER on purpose: anything
        // that parses this instead of comparing it collapses distinct lines.
        line_id: '1757419200000000001',
      },
    ],
    next_cursor: 'next-token',
  },
}
/** Store-backed facet counts — no longer derived from the returned page. */
const logFacets = {
  facets: {
    level: [
      { value: 'INFO', count: 40 },
      { value: 'ERROR', count: 12 },
    ],
    project_id: [{ value: '1', count: 52 }],
    external_service_id: [],
    env: [
      { value: 'production', count: 50 },
      { value: 'staging', count: 2 },
    ],
    node_id: [{ value: '7', count: 30 }],
    deploy_id: [{ value: '42', count: 25 }],
  },
  partial: false,
}
const endpoints = {
  analytics: '/api/analytics/global',
  traces: '/api/otel/global/trace-summaries',
  errors: '/api/error-groups',
  logs: '/api/logs/global/search',
  logFacets: '/api/logs/global/facets',
}
type Kind = keyof typeof fixtures

/**
 * A search page whose cursor terminates: the first page offers `next-token`,
 * the cursored page ends the walk. Infinite scroll auto-loads whenever the
 * rope fits the viewport, so a fixture that always hands back the same cursor
 * would spin forever.
 */
function logPage(
  route: Parameters<Parameters<Page['route']>[1]>[0],
  lines: unknown[] = fixtures.logs.lines
) {
  const cursor = route.request().postDataJSON()?.cursor
  return route.fulfill({
    json: {
      lines: cursor ? [] : lines,
      next_cursor: cursor ? null : 'next-token',
    },
  })
}

/** The time window each list request asked the backend for, in order. */
type QueriedWindow = { from: string | null; to: string | null }

async function mock(page: Page, kind: Kind) {
  const windows: QueriedWindow[] = []
  await page.route(/\/api\/projects(?:\?|$)/, (route) =>
    route.fulfill({
      json: {
        projects: [{ id: 1, name: 'Storefront', slug: 'storefront' }],
        total: 1,
      },
    })
  )
  await page.route(
    (url) => url.pathname === endpoints.logFacets,
    (route) => route.fulfill({ json: logFacets })
  )
  await page.route(
    (url) => url.pathname === endpoints[kind],
    (route) => {
      const request = route.request()
      const query = new URL(request.url()).searchParams
      const body = request.method() === 'POST' ? request.postDataJSON() : {}
      windows.push({
        from:
          query.get('start_date') ??
          query.get('start_time') ??
          body?.start_time,
        to: query.get('end_date') ?? query.get('end_time') ?? body?.end_time,
      })
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
      if (kind === 'logs') return logPage(route)
      return route.fulfill({ json: fixtures[kind] })
    }
  )
  return windows
}
for (const kind of Object.keys(fixtures) as Kind[]) {
  for (const width of [1440, 390]) {
    test(`${kind} global page renders and preserves scope at ${width}px`, async ({
      page,
    }) => {
      const errors: string[] = []
      page.on('pageerror', (error) => errors.push(error.message))
      await page.setViewportSize({ width, height: 1000 })
      const windows = await mock(page, kind)
      await page.goto(`/${kind}`)
      await expect(
        page.getByRole('heading', {
          name: kind[0].toUpperCase() + kind.slice(1),
          exact: true,
        })
      ).toBeVisible()
      await expect(
        kind === 'analytics'
          ? page
              .getByRole('region', { name: 'Top Pages', exact: true })
              .getByText('Storefront', { exact: true })
          : page.getByRole('cell', { name: 'Storefront', exact: false }).first()
      ).toBeVisible()
      if (kind === 'logs') {
        await page
          .getByRole('combobox', { name: 'Search log messages' })
          .fill('project:')
        await page.getByRole('option', { name: /project:Storefront/ }).click()
      } else {
        const filters = page.getByRole('region', {
          name: `${kind[0].toUpperCase() + kind.slice(1)} filters`,
        })
        await filters.getByRole('combobox', { name: 'Project scope' }).click()
        await page.getByRole('option', { name: /Storefront/ }).click()
      }
      await expect(page).toHaveURL(/project_id=1/)
      for (const preset of ['1h', '6h', '24h', '7d'])
        await expect(
          page.getByRole('button', { name: preset, exact: true })
        ).toBeVisible()
      await page.getByRole('button', { name: '6h', exact: true }).click()
      await expect(page).toHaveURL(/range=6h/)
      // Quick presets are rolling: the URL carries only the preset, and each
      // load resolves a fresh 6h window that is what the backend is queried for.
      const bounds = new URL(page.url()).searchParams
      expect(bounds.has('from')).toBe(false)
      expect(bounds.has('to')).toBe(false)
      const span = (queried?: QueriedWindow) =>
        queried && Date.parse(queried.to!) - Date.parse(queried.from!)
      await expect
        .poll(() => span(windows[windows.length - 1]))
        .toBe(6 * 3600000)
      const requestsBeforeReload = windows.length
      const windowBeforeReload = windows[requestsBeforeReload - 1]
      await page.reload()
      if (kind === 'logs')
        await expect(
          page.getByRole('button', { name: 'Edit project filter' })
        ).toContainText('Storefront')
      else
        await expect(
          page
            .getByRole('region', {
              name: `${kind[0].toUpperCase() + kind.slice(1)} filters`,
            })
            .getByRole('combobox', { name: 'Project scope' })
        ).toContainText('Storefront')
      const reloaded = new URL(page.url()).searchParams
      expect(reloaded.get('range')).toBe('6h')
      expect(reloaded.has('from')).toBe(false)
      // A rolling preset must resolve a new window on reload, not replay the
      // bounds it queried before.
      await expect
        .poll(() => {
          if (windows.length <= requestsBeforeReload) return undefined
          const latest = windows[windows.length - 1]
          return {
            span: span(latest),
            advanced:
              Date.parse(latest.from!) > Date.parse(windowBeforeReload.from!) &&
              Date.parse(latest.to!) > Date.parse(windowBeforeReload.to!),
          }
        })
        .toEqual({ span: 6 * 3600000, advanced: true })
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth
        )
      ).toBe(true)
      if (kind === 'traces')
        await expect(
          page.getByRole('link', { name: 'Cross-project waterfall' })
        ).toHaveAttribute(
          'href',
          new RegExp(`^/traces/global/${traceId}\\?start_time=.+&end_time=.+`)
        )
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
          : kind === 'logs'
            ? logPage(route)
            : route.fulfill({ json: fixtures[kind] })
    )
    await page.goto(`/${kind}`)
    const retry = page.getByRole('button', {
      name: `Retry ${kind === 'analytics' ? 'analytics metrics' : kind}`,
      exact: true,
    })
    await expect(retry).toBeVisible()
    await expect(page.getByText('Access denied').first()).toBeVisible()
    failed = false
    await retry.click()
    await expect(
      kind === 'analytics'
        ? page
            .getByRole('region', { name: 'Top Pages', exact: true })
            .getByText('Storefront', { exact: true })
        : page.getByRole('cell', { name: 'Storefront', exact: false }).first()
    ).toBeVisible()
  })
}
test('loading older lines walks the keyset cursor in the same window and a filter change restarts it', async ({
  page,
}) => {
  await mock(page, 'logs')
  await page.goto('/logs')
  const first = await page.waitForRequest(
    (req) => new URL(req.url()).pathname === endpoints.logs
  )
  const window = first.postDataJSON()
  // Page size is not user-configurable: we always ask for the server default.
  expect(window.page_size).toBe(200)
  // Infinite scroll walks the cursor on its own once the rope fits the
  // viewport; the footer button is the same action for anyone not scrolling.
  const older = await page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname === endpoints.logs &&
      req.postDataJSON().cursor === 'next-token'
  )
  const olderBody = older.postDataJSON()
  expect(olderBody.start_time).toBe(window.start_time)
  expect(olderBody.end_time).toBe(window.end_time)
  expect(olderBody.page_size).toBe(200)
  // A terminating cursor is a real end-of-results, not an exhausted budget.
  await expect(page.getByText('End of results for this range')).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Load older lines' })
  ).toHaveCount(0)
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
test('a refused query is reported as an error, never as "some logs may be missing"', async ({
  page,
}) => {
  await mock(page, 'logs')
  await page.route(
    (url) => url.pathname === endpoints.logs,
    (route) =>
      route.fulfill({
        status: 503,
        json: {
          title: 'Log store unavailable',
          detail: 'Log store unavailable',
        },
      })
  )
  await page.goto('/logs')
  await expect(page.getByText('Log store unavailable').first()).toBeVisible()
  await expect(
    page.getByRole('button', { name: 'Retry logs', exact: true })
  ).toBeVisible()
  // The old scan-limit/partial-results vocabulary described a state the
  // indexed store cannot be in when the request outright failed.
  await expect(page.getByText('Partial results')).toHaveCount(0)
  await expect(page.getByText('Search limit reached')).toHaveCount(0)
  await expect(page.getByText('Search paused')).toHaveCount(0)
  await expect(
    page.getByText(/some matching logs may be missing/i)
  ).toHaveCount(0)
})

for (const width of [1440, 390]) {
  test(`log presentation modes keep working at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1000 })
    await mock(page, 'logs')
    await page.route(
      (url) => url.pathname === endpoints.logs,
      (route) =>
        route.fulfill({
          json: {
            ...fixtures.logs,
            partial: true,
            scanned_back_to: stamp,
            next_cursor: 'partial-next-token',
          },
        })
    )
    await page.goto('/logs')
    const warning = page
      .getByRole('status')
      .filter({ hasText: 'Search paused' })
    await expect(warning).toBeVisible()
    await expect(warning).toContainText('Searched back to')
    const keepSearching = warning.getByRole('button', {
      name: 'Keep searching',
    })
    await expect(keepSearching).toBeEnabled()
    const checkoutLine = page.getByRole('button', {
      name: 'Inspect log: Checkout request failed',
      exact: true,
    })
    await expect(checkoutLine).toHaveCount(1)
    await expect(checkoutLine).toBeVisible()
    await expect(
      page.getByText('1 loaded line · newest first', { exact: false }).first()
    ).toBeVisible()
    // A partial page must not disable the normal "keep paging" affordance —
    // pressing Next/Load older is exactly how the user resumes the search.
    await expect(
      page.getByRole('button', { name: 'Load older lines', exact: true })
    ).toBeEnabled()
    await expect(page.getByText('No logs in this view')).toHaveCount(0)
    await page.screenshot({
      path: `/tmp/temps-log-explorer-${width}.png`,
      fullPage: true,
    })
    await page.getByRole('button', { name: 'Patterns', exact: true }).click()
    await expect(
      page.getByText('Checkout request failed', { exact: true })
    ).toBeVisible()
    await page.getByRole('button', { name: 'By service', exact: true }).click()
    await expect(
      page.getByRole('button', { name: 'Storefront / web', exact: true })
    ).toBeVisible()
  })
}

for (const width of [1440, 390]) {
  test(`custom range validates, applies, and survives reload at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 1000 })
    if (width === 1440)
      await page.addInitScript(() => localStorage.setItem('theme', 'dark'))
    await mock(page, 'logs')
    await page.goto('/logs')
    await expect(page).toHaveURL(/[?&]range=1d(?:&|$)/)
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
    await page.getByRole('button', { name: '24h', exact: true }).click()
    await expect(page).toHaveURL(/range=1d/)
    await expect(
      page.getByRole('button', { name: '24h', exact: true })
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
      const pages = page.getByRole('region', {
        name: 'Top Pages',
        exact: true,
      })
      await expect(pages).toContainText('Storefront')
      const bounds = await pages.boundingBox()
      expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width)
    }
    const tabs = page.getByRole('tablist', { name: 'Analytics breakdowns' })
    await tabs.getByRole('tab', { name: 'Audience' }).click()
    await expect(tabs.getByRole('tab', { name: 'Audience' })).toHaveAttribute(
      'aria-selected',
      'true'
    )
    await tabs.getByRole('tab', { name: 'Audience' }).press('ArrowLeft')
    await expect(tabs.getByRole('tab', { name: 'Traffic' })).toHaveAttribute(
      'aria-selected',
      'true'
    )
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
      logPage(route, [
        {
          ...fixtures.logs.lines[0],
          node_id: 7,
          node_name: 'worker-7',
          fields: { request_id: 'request-test', duration_ms: 250 },
        },
      ])
  )
  await page.goto('/logs')
  await expect(
    page.getByRole('complementary', { name: 'Log facets' })
  ).toContainText('Counts across the whole time range')
  // 30 is the store's count for node 7, not the 1 line on screen.
  await expect(
    page.getByRole('region', { name: 'Node facets', exact: true })
  ).toContainText('30')
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

test('logs workspace supports grouping, columns and facets without a page-only volume chart', async ({
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
          lines: Array.from({ length: 60 }, (_, i) => ({
            ...fixtures.logs.lines[0],
            line_id: String(1757419200000000001n + BigInt(i)),
            timestamp: new Date(Date.parse(stamp) - i * 30000).toISOString(),
            level: i % 7 === 0 ? 'ERROR' : i % 3 === 0 ? 'WARN' : 'INFO',
            message:
              i % 7 === 0
                ? 'Checkout request failed'
                : `Completed GET /products in ${20 + i}ms`,
            deploy_id: 42,
            node_id: 7,
            node_name: 'worker-7',
          })),
        },
      })
  )
  await page.goto(
    '/logs?range=custom&from=2026-09-09T11:00:00Z&to=2026-09-09T13:00:00Z'
  )
  await expect(page.getByText('Volume by level', { exact: true })).toHaveCount(
    0
  )
  await expect(
    page.getByRole('columnheader', { name: 'deployment', exact: true })
  ).toBeVisible()
  await page.screenshot({
    path: '/tmp/temps-logs-reference-desktop.png',
    fullPage: true,
  })
  await page.getByRole('button', { name: 'Patterns', exact: true }).click()
  await expect(
    page.getByText('Exact repeated messages on this loaded page.', {
      exact: false,
    })
  ).toBeVisible()
  await page.getByRole('button', { name: 'By service', exact: true }).click()
  await expect(
    page.getByRole('button', { name: 'Storefront / web', exact: true })
  ).toBeVisible()
  await page.getByRole('button', { name: 'List', exact: true }).click()
  await page.getByRole('button', { name: 'Columns', exact: true }).click()
  await page
    .getByRole('menuitemcheckbox', { name: 'node', exact: true })
    .click()
  await page.keyboard.press('Escape')
  await expect(
    page.getByRole('columnheader', { name: 'node', exact: true })
  ).toBeVisible()
  await page.getByRole('textbox', { name: 'Filter facets' }).fill('worker')
  await expect(page.getByRole('region', { name: 'Node facets' })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Level facets' })).toHaveCount(
    0
  )
  await expect(page.getByRole('region', { name: 'Log volume' })).toHaveCount(0)
})

test('log key:value autocomplete applies, edits and validates filters', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1000 })
  await mock(page, 'logs')
  await page.goto('/logs')
  const input = page.getByRole('combobox', { name: 'Search log messages' })
  await expect(
    page.getByRole('combobox', { name: 'Project scope' })
  ).toHaveCount(0)
  await expect(page.getByRole('combobox', { name: 'Log level' })).toHaveCount(0)
  await input.fill('lev')
  await input.press('Enter')
  await expect(input).toHaveValue('level:')
  await input.press('Enter')
  await expect(
    page.getByRole('button', { name: 'Edit level filter' })
  ).toHaveText('level:error')
  const request = page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname === endpoints.logs &&
      req.postDataJSON().levels?.[0] === 'WARN' &&
      req.postDataJSON().envs?.[0] === 'Production'
  )
  await input.fill('level:warn env:Production timeout')
  await input.press('Enter')
  const body = (await request).postDataJSON()
  expect(body.text).toBe('timeout')
  expect(body.cursor).toBeUndefined()
  await expect(page).not.toHaveURL(/cursor=/)
  await page.reload()
  await expect(
    page.getByRole('button', { name: 'Edit env filter' })
  ).toHaveText('env:Production')
  await page.getByRole('button', { name: 'Edit level filter' }).click()
  await input.fill('level:info')
  await input.press('Enter')
  await expect(
    page.getByRole('button', { name: 'Edit level filter' })
  ).toHaveText('level:info')
  await input.fill('node:invalid')
  await input.press('Enter')
  await expect(page.getByRole('alert')).toContainText('positive numeric ID')
  await expect(page).not.toHaveURL(/node_id=/)
  await input.press('Escape')
  await expect(input).toHaveAttribute('aria-expanded', 'false')
  await input.fill('project:store')
  await page.getByRole('option', { name: /project:Storefront/ }).click()
  await expect(page).toHaveURL(/project_id=1/)
  await input.fill('source:service')
  await input.press('Enter')
  await expect(page).toHaveURL(/source=service/)
  await expect(page).not.toHaveURL(/project_id=/)
  await page.getByRole('button', { name: 'Clear source: service' }).click()
  await expect(page).not.toHaveURL(/source=/)
  await input.fill('level:')
  await page.screenshot({
    path: '/tmp/temps-logs-key-value-autocomplete.png',
    fullPage: true,
  })
})

test('log autocomplete remains usable on a narrow screen', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await mock(page, 'logs')
  await page.goto('/logs')
  const input = page.getByRole('combobox', { name: 'Search log messages' })
  await input.fill('env:')
  await page.getByRole('option', { name: /^env:production/ }).click()
  await expect(
    page.getByRole('button', { name: 'Edit env filter' })
  ).toHaveText('env:production')
  await input.fill('level:')
  await input.press('ArrowDown')
  await input.press('Tab')
  await expect(
    page.getByRole('button', { name: 'Edit level filter' })
  ).toHaveText('level:warn')
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth
    )
  ).toBe(true)
})

for (const theme of ['light', 'dark']) {
  test(`logs render ANSI colors safely in ${theme}`, async ({ page }) => {
    await page.addInitScript(
      (theme) => localStorage.setItem('theme', theme),
      theme
    )
    await mock(page, 'logs')
    await page.route('**/api/logs/global/search**', (route) =>
      route.fulfill({
        json: {
          ...fixtures.logs,
          lines: [0, 1, 2].map((i) => ({
            ...fixtures.logs.lines[0],
            timestamp: new Date(Date.parse(stamp) + i * 60000).toISOString(),
            line_id: String(1757419200000000001n + BigInt(i)),
            message:
              '\u001b[31mError <script>unsafe</script>\u001b[0m plain text',
          })),
        },
      })
    )
    await page.goto('/logs')
    const message = page
      .getByText('Error <script>unsafe</script>', { exact: true })
      .first()
    await expect(message).toBeVisible()
    await expect(message).toHaveCSS('color', 'rgb(170, 0, 0)')
    await expect(page.getByRole('region', { name: 'Log volume' })).toHaveCount(
      0
    )
    await expect(page.locator('html')).toHaveClass(new RegExp(theme))
    await page.screenshot({ path: `/tmp/temps-logs-ansi-${theme}.png` })
  })
}

test('facet suggestions come from the store, including values absent from the loaded page', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1000 })
  await mock(page, 'logs')
  await page.route(
    (url) => url.pathname === endpoints.logFacets,
    (route) => route.fulfill({ json: { ...logFacets, partial: true } })
  )
  await page.goto('/logs')
  // `staging` never appears in any returned line — the old page-derived
  // autocomplete could not have offered it.
  const input = page.getByRole('combobox', { name: 'Search log messages' })
  await input.fill('env:')
  await expect(page.getByRole('option', { name: /^env:staging/ })).toBeVisible()
  const request = page.waitForRequest(
    (req) =>
      new URL(req.url()).pathname === endpoints.logs &&
      req.postDataJSON().envs?.[0] === 'staging'
  )
  await page.getByRole('option', { name: /^env:staging/ }).click()
  await request
  await expect(page).toHaveURL(/env=staging/)
  // A capped value list is said out loud rather than passed off as complete.
  await page.keyboard.press('Escape')
  await expect(
    page.getByRole('complementary', { name: 'Log facets' })
  ).toContainText('capped')
})
