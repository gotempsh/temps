// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

async function mockCrawlerPage(page: Page) {
  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url())
    let json: unknown = []
    if (url.pathname === '/api/user/me')
      json = {
        id: 42,
        name: 'Test Operator',
        email: 'operator@example.com',
        role: 'admin',
      }
    else if (url.pathname === '/api/projects/by-slug/crawler-test')
      json = {
        id: 2,
        slug: 'crawler-test',
        name: 'Crawler test',
        preset: 'dockerfile',
        deployment_config: {},
        main_branch: 'main',
        directory: '.',
      }
    else if (url.pathname.endsWith('/last-deployment')) json = null
    else if (url.pathname.endsWith('/active-visitors')) json = { count: 0 }
    else if (url.pathname === '/api/proxy-logs') {
      const hours =
        (Date.parse(url.searchParams.get('end_date') ?? '') -
          Date.parse(url.searchParams.get('start_date') ?? '')) /
        3600000
      // Simulate the backend's hidden one-hour default, with older requests
      // only returned when the caller explicitly asks for a wider window.
      const total = !Number.isFinite(hours) || hours <= 1 ? 43 : 143
      const currentPage = Number(url.searchParams.get('page') ?? 1)
      const pageSize = Number(url.searchParams.get('page_size') ?? 50)
      json = {
        total,
        page: currentPage,
        page_size: pageSize,
        total_pages: Math.ceil(total / pageSize),
        logs: [
          {
            request_id: `request-${currentPage}`,
            timestamp: new Date().toISOString(),
            bot_name: 'SyntheticCrawler',
            method: 'GET',
            host: 'example.test',
            path: `/page-${currentPage}`,
            status_code: 200,
          },
        ],
      }
    }
    await route.fulfill({ json, contentType: 'application/json' })
  })
}

function waitForLogs(page: Page, match: (query: URLSearchParams) => boolean) {
  return page.waitForRequest((request) => {
    const url = new URL(request.url())
    return url.pathname === '/api/proxy-logs' && match(url.searchParams)
  })
}

for (const width of [1440, 390]) {
  test(`crawler ranges fetch older requests and preserve filters at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 1000 })
    const errors: string[] = []
    page.on('pageerror', (error) => errors.push(error.message))
    await mockCrawlerPage(page)
    const initial = waitForLogs(
      page,
      (q) => q.has('start_date') && q.has('end_date')
    )
    await page.goto(
      '/projects/crawler-test/ai-crawlers?ai_provider=SyntheticProvider&ai_agent=SyntheticCrawler&path=%2Fpage&page_size=25'
    )
    const first = new URL((await initial).url()).searchParams
    expect(
      Date.parse(first.get('end_date')!) - Date.parse(first.get('start_date')!)
    ).toBe(24 * 3600000)
    await expect(page.getByText('143 requests in selected range')).toBeVisible()
    const control = page.getByRole('group', {
      name: 'Date and time range',
      exact: true,
    })
    await expect(
      control.getByRole('button', { name: '24h', exact: true })
    ).toHaveAttribute('aria-pressed', 'true')
    const paging = waitForLogs(page, (q) => q.get('page') === '2')
    await page
      .getByRole('button', { name: /^(Go to n|N)ext page$/ })
      .filter({ visible: true })
      .click()
    const second = new URL((await paging).url()).searchParams
    expect(second.get('start_date')).toBe(first.get('start_date'))
    expect(second.get('end_date')).toBe(first.get('end_date'))
    for (const [preset, hours] of [
      ['1h', 1],
      ['6h', 6],
      ['7d', 168],
    ] as const) {
      const request = waitForLogs(
        page,
        (q) =>
          Date.parse(q.get('end_date') ?? '') -
            Date.parse(q.get('start_date') ?? '') ===
          hours * 3600000
      )
      await control.getByRole('button', { name: preset, exact: true }).click()
      const q = new URL((await request).url()).searchParams
      expect(q.get('page')).toBe('1')
      expect(q.get('project_id')).toBe('2')
      expect(q.get('is_ai_agent')).toBe('true')
      expect(q.get('ai_provider')).toBe('SyntheticProvider')
      expect(q.get('ai_agent')).toBe('SyntheticCrawler')
      expect(q.get('path')).toBe('/page')
      expect(q.get('page_size')).toBe('25')
      await expect(
        page.getByText(`${hours === 1 ? 43 : 143} requests in selected range`)
      ).toBeVisible()
    }
    await page.reload()
    await expect(
      control.getByRole('button', { name: '7d', exact: true })
    ).toHaveAttribute('aria-pressed', 'true')
    const beforeRefresh = new URL(page.url()).searchParams
    const refresh = waitForLogs(page, () => true)
    await page
      .getByRole('button', { name: 'Refresh AI crawler activity' })
      .click()
    await refresh
    expect(new URL(page.url()).searchParams.get('range')).toBe(
      beforeRefresh.get('range')
    )
    const box = await control.boundingBox()
    expect(box!.x + box!.width).toBeLessThanOrEqual(width)
    expect(errors).toEqual([])
    await page.screenshot({
      path: testInfo.outputPath(`crawler-${width}.png`),
      fullPage: true,
    })
  })
}

test('crawler custom dates survive reload and empty and failed requests are distinct', async ({
  page,
}) => {
  await mockCrawlerPage(page)
  await page.goto('/projects/crawler-test/ai-crawlers?page_size=200')
  await expect(page.getByText('143 requests in selected range')).toBeVisible()
  const control = page.getByRole('group', {
    name: 'Date and time range',
    exact: true,
  })
  await control.getByRole('button', { name: 'Custom time range' }).click()
  await page
    .getByLabel('Start date and time', { exact: true })
    .fill('2026-09-01T09:30')
  await page
    .getByLabel('End date and time', { exact: true })
    .fill('2026-09-03T17:45')
  const request = waitForLogs(
    page,
    (q) =>
      Date.parse(q.get('end_date') ?? '') -
        Date.parse(q.get('start_date') ?? '') ===
      (56 * 60 + 15) * 60000
  )
  await page.getByRole('button', { name: 'Apply range', exact: true }).click()
  const query = new URL((await request).url()).searchParams
  expect(query.get('page_size')).toBe('50')
  expect(
    Date.parse(query.get('end_date')!) - Date.parse(query.get('start_date')!)
  ).toBe((56 * 60 + 15) * 60000)
  const reload = waitForLogs(page, () => true)
  await page.reload()
  const reloaded = new URL((await reload).url()).searchParams
  expect(reloaded.get('start_date')).toBe(query.get('start_date'))
  expect(reloaded.get('end_date')).toBe(query.get('end_date'))
  await expect(
    control.getByRole('button', { name: 'Custom time range' })
  ).toHaveAttribute('aria-pressed', 'true')
  await page.route('**/api/proxy-logs?**', (route) =>
    route.fulfill({
      json: { logs: [], total: 0, total_pages: 0, page: 1, page_size: 50 },
    })
  )
  await page
    .getByRole('button', { name: 'Refresh AI crawler activity' })
    .click()
  await expect(page.getByText('0 requests in selected range')).toBeVisible()
  await expect(
    page.getByText('No AI crawler activity in this range')
  ).toBeVisible()
  await page.route('**/api/proxy-logs?**', (route) =>
    route.fulfill({ status: 400, json: { detail: 'Invalid range' } })
  )
  await page
    .getByRole('button', { name: 'Refresh AI crawler activity' })
    .click()
  await expect(
    page.getByText('Failed to load AI crawler activity. Please try again.')
  ).toBeVisible()
  await expect(page.getByText('0 requests in selected range')).not.toBeVisible()
})

test('delayed preset selections use current time atomically and paging keeps it', async ({
  page,
}) => {
  const mounted = new Date('2026-09-14T10:00:00Z')
  await page.clock.setFixedTime(mounted)
  await mockCrawlerPage(page)
  await page.goto('/projects/crawler-test/ai-crawlers')
  await expect(page.getByText('143 requests in selected range')).toBeVisible()
  const control = page.getByRole('group', {
    name: 'Date and time range',
    exact: true,
  })
  const requests: URLSearchParams[] = []
  page.on('request', (request) => {
    const url = new URL(request.url())
    if (url.pathname === '/api/proxy-logs') requests.push(url.searchParams)
  })
  for (const [preset, hours, end] of [
    ['6h', 6, '2026-09-14T11:00:00.000Z'],
    ['6h', 6, '2026-09-14T12:00:00.000Z'],
    ['24h', 24, '2026-09-14T13:00:00.000Z'],
  ] as const) {
    await page.clock.setFixedTime(new Date(end))
    requests.length = 0
    const response = page.waitForResponse(
      (r) => new URL(r.url()).pathname === '/api/proxy-logs'
    )
    await control.getByRole('button', { name: preset, exact: true }).click()
    await response
    await expect(page.getByText('143 requests in selected range')).toBeVisible()
    expect(requests).toHaveLength(1)
    expect(requests[0].get('end_date')).toBe(end)
    expect(Date.parse(requests[0].get('start_date')!)).toBe(
      Date.parse(end) - hours * 3600000
    )
    const paging = waitForLogs(page, (q) => q.get('page') === '2')
    await page
      .getByRole('button', { name: /^(Go to n|N)ext page$/ })
      .filter({ visible: true })
      .click()
    expect(new URL((await paging).url()).searchParams.get('end_date')).toBe(end)
    await expect(
      page.getByText('example.test/page-2', { exact: false })
    ).toBeVisible()
  }
})
