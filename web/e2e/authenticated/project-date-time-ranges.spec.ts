// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from '@playwright/test'

const cases = [
  ['traces', '/api/otel/trace-summaries', 'start_time', 'end_time'],
  ['traces/operations', '/api/otel/span-stats', 'start_time', 'end_time'],
  ['telemetry-logs', '/api/otel/logs', 'start_time', 'end_time'],
  ['analytics', '/hourly-visits', 'start_date', 'end_date'],
  ['errors', '/error-groups', 'start_date', 'end_date'],
] as const

for (const [path, endpoint, fromKey, toKey] of cases) {
  for (const width of [1440, 390]) {
    test(`${path} applies quick and custom times at ${width}px`, async ({
      page,
    }) => {
      const response = await page.request.get('/api/projects')
      const { projects } = await response.json()
      const project = projects[0]
      test.skip(!project, 'Requires a local test project')
      const pageErrors: string[] = []
      page.on('pageerror', (error) => pageErrors.push(error.message))
      await page.setViewportSize({ width, height: 1000 })
      await page.route(/\/has-events(?:\?|$)/, (route) =>
        route.fulfill({ json: { has_events: true } })
      )
      await page.route(/\/has-error-groups(?:\?|$)/, (route) =>
        route.fulfill({ json: { has_error_groups: true } })
      )
      await page.goto(`/projects/${project.slug}/${path}`)
      const control = page.getByRole('group', {
        name: 'Date and time range',
        exact: true,
      })
      await expect(control).toBeVisible()
      for (const preset of ['1h', '6h', '1d', '7d'])
        await expect(
          control.getByRole('button', { name: preset, exact: true })
        ).toBeVisible()
      const sixHours = page.waitForRequest((request) => {
        const url = new URL(request.url())
        return (
          url.pathname.endsWith(endpoint) &&
          Date.parse(url.searchParams.get(toKey) ?? '') -
            Date.parse(url.searchParams.get(fromKey) ?? '') ===
            6 * 3600000
        )
      })
      await control.getByRole('button', { name: '6h', exact: true }).click()
      await sixHours
      await expect(
        control.getByRole('button', { name: '6h', exact: true })
      ).toHaveAttribute('aria-pressed', 'true')
      await control.getByRole('button', { name: 'Custom time range' }).click()
      await page
        .getByLabel('Start date and time', { exact: true })
        .fill('2026-09-07T09:30')
      await page
        .getByLabel('End date and time', { exact: true })
        .fill('2026-09-08T17:45')
      const expected = await page.evaluate(() => ({
        from: new Date(2026, 8, 7, 9, 30).toISOString(),
        to: new Date(2026, 8, 8, 17, 45).toISOString(),
      }))
      const custom = page.waitForRequest((request) => {
        const url = new URL(request.url())
        return (
          url.pathname.endsWith(endpoint) &&
          url.searchParams.get(fromKey) === expected.from &&
          url.searchParams.get(toKey) === expected.to
        )
      })
      await page
        .getByRole('button', { name: 'Apply range', exact: true })
        .click()
      await custom
      await expect(
        control.getByRole('button', { name: 'Custom time range' })
      ).toHaveAttribute('aria-pressed', 'true')
      await page.reload()
      await expect(
        control.getByRole('button', { name: 'Custom time range' })
      ).toHaveAttribute('aria-pressed', 'true')
      expect(pageErrors).toEqual([])
      const box = await control.boundingBox()
      expect(box!.x + box!.width).toBeLessThanOrEqual(width)
      await page.screenshot({
        path: `/tmp/temps-project-${path.replace(/\//g, '-')}-range-${width}.png`,
      })
    })
  }
}

for (const path of [
  '/audit-logs',
  '/revenue',
  '/proxy',
  '/proxy-logs',
  '/settings/otel-pipeline',
  '/ai-gateway/usage',
]) {
  test(`${path} uses the compact range at mobile width`, async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 1000 })
    await page.goto(path)
    const control = page
      .getByRole('group', { name: 'Date and time range', exact: true })
      .first()
    await expect(control).toBeVisible()
    await control.getByRole('button', { name: '6h', exact: true }).click()
    await expect(
      control.getByRole('button', { name: '6h', exact: true })
    ).toHaveAttribute('aria-pressed', 'true')
    await control.getByRole('button', { name: 'Custom time range' }).click()
    await expect(
      page.getByLabel('Start date and time', { exact: true })
    ).toBeVisible()
    await page.getByRole('button', { name: 'Cancel', exact: true }).click()
    await expect(
      control.getByRole('button', { name: '6h', exact: true })
    ).toHaveAttribute('aria-pressed', 'true')
    const box = await control.boundingBox()
    expect(box!.x + box!.width).toBeLessThanOrEqual(390)
    await page.screenshot({
      path: `/tmp/temps-range${path.replace(/\//g, '-')}-390.png`,
    })
  })
}

for (const [path, parameter, endpoint] of [
  ['traces', 'q', '/api/otel/trace-summaries'],
  ['telemetry-logs', 'trace', '/api/otel/logs'],
] as const) {
  test(`${path} keeps exact trace searches independent of time`, async ({
    page,
  }) => {
    const { projects } = await (await page.request.get('/api/projects')).json()
    test.skip(!projects[0], 'Requires a local test project')
    const traceId = 'a'.repeat(32)
    const request = page.waitForRequest((request) => {
      const url = new URL(request.url())
      return (
        url.pathname === endpoint &&
        url.searchParams.get('trace_id') === traceId
      )
    })
    await page.goto(
      `/projects/${projects[0].slug}/${path}?${parameter}=${traceId}&range=1h`
    )
    const query = new URL((await request).url()).searchParams
    expect(query.has('start_time')).toBe(false)
    expect(query.has('end_time')).toBe(false)
    if (path === 'telemetry-logs')
      await expect(
        page
          .getByRole('group', { name: 'Date and time range', exact: true })
          .getByRole('button', { name: '6h', exact: true })
      ).toBeDisabled()
  })
}
