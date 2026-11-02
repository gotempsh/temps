// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test, type Page } from '@playwright/test'

const member = {
  container_id: 'abc123',
  container_name: 'redis-example',
  role: 'standalone',
  status: 'running',
  started_at: '2026-10-01T00:00:00Z',
  restart_count: 0,
  oom_killed: false,
  resource_limits: { shm_mb: 64 },
}

const workload = {
  service_id: 23,
  service_name: 'redis-example',
  service_type: 'redis',
  mode: 'legacy',
  node_id: null,
  support: { supported: true },
}

async function openServicePage(page: Page, width: number) {
  await page.setViewportSize({ width, height: 1000 })
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname.replace(/^\/api/, '')
    let body: unknown = []
    if (path === '/user/me')
      body = {
        id: 1,
        name: 'Operator',
        username: 'operator',
        email: 'operator@example.test',
        role: 'admin',
        mfa_enabled: false,
      }
    else if (path === '/operations')
      // The header's operations tray reads a page object.
      body = { operations: [], total: 0, page: 1, per_page: 20, total_pages: 0 }
    else if (path === '/external-services/23')
      body = {
        service: {
          id: 23,
          name: 'redis-example',
          service_type: 'redis',
          status: 'running',
          topology: 'standalone',
          metrics_enabled: true,
          created_at: '2026-10-01T00:00:00Z',
          updated_at: '2026-10-01T00:00:00Z',
        },
        current_parameters: { host: 'redis.example.test', port: '6379' },
        sensitive_parameters: [],
      }
    else if (path === '/backups/external-services/23/backups')
      body = { backups: [], total: 0, page: 1, page_size: 20 }
    else if (path === '/external-services/23/health-status')
      body = {
        service_id: 23,
        status: 'healthy',
        consecutive_failures: 0,
        uptime_24h_percent: 100,
        response_time_ms: 2,
        last_checked_at: new Date().toISOString(),
        recent_checks: [],
      }
    else if (path === '/external-services/23/workload-mode') body = workload
    else if (path === '/external-services/23/workload-mode/pending-changes')
      body = {
        ...workload,
        container_found: true,
        container_name: 'redis-example',
        computed_at: new Date().toISOString(),
        changes: [
          {
            field: 'labels',
            current: 'none',
            after_recreate: Array.from(
              { length: 6 },
              (_, index) => `sh.temps.example-label-${index}=value`
            ).join(', '),
          },
          { field: 'init', current: 'false', after_recreate: 'true' },
        ],
      }
    else if (path === '/external-services/23/stats')
      body = {
        service_id: 23,
        topology: 'standalone',
        members: [
          {
            container_name: member.container_name,
            role: member.role,
            cpu_percent: 2.2,
            memory_usage_bytes: 69_000_000,
          },
        ],
      }
    else if (path === '/external-services/23/runtime')
      body = { service_id: 23, topology: 'standalone', members: [member] }
    else if (path === '/external-services/23/metrics/status')
      body = {
        enabled: true,
        last_received_at: new Date().toISOString(),
        last_scraped_at: new Date().toISOString(),
      }
    else if (path === '/external-services/23/metrics/latest')
      body = {
        'container.cpu_percent': 2.2,
        'container.memory_used_bytes': 69_500_000,
        'redis.memory_used_bytes': 1_800_000,
        'redis.keyspace_hit_ratio': 1,
        'redis.evicted_keys_total': 0,
        'redis.connected_clients': 2,
      }
    await route.fulfill({ json: body })
  })
  await page.goto('/storage/23')
  await expect(page.getByText('Current service parameters')).toBeVisible()
}

/** The Configuration card is the aside; the resources row is in the main column. */
const aside = (page: Page) =>
  page.locator('div.min-w-0.space-y-6', {
    has: page.getByText('Current service parameters'),
  })
const mainColumn = (page: Page) =>
  page.locator('div.min-w-0.space-y-6', { has: page.getByText('Monitoring') })

test('the way back to the list is the breadcrumb, not a second arrow in the header', async ({
  page,
}) => {
  await openServicePage(page, 1153)

  await expect(
    page
      .getByRole('navigation', { name: 'breadcrumb' })
      .getByRole('link', { name: 'Databases' })
  ).toHaveAttribute('href', '/storage')
  await expect(page.getByRole('link', { name: 'Back to storage' })).toHaveCount(
    0
  )
})

test('beside a sidebar, the service page drops to one column so nothing is squeezed', async ({
  page,
}) => {
  await openServicePage(page, 1153)

  const main = await mainColumn(page).last().boundingBox()
  const side = await aside(page).last().boundingBox()
  expect(main && side).toBeTruthy()
  // Stacked: the aside starts under the main column and shares its width.
  expect(side!.y).toBeGreaterThanOrEqual(main!.y + main!.height)
  expect(Math.abs(side!.width - main!.width)).toBeLessThan(2)
})

test('with room to spare, the configuration sits beside the main column', async ({
  page,
}) => {
  await openServicePage(page, 1700)

  const main = await mainColumn(page).last().boundingBox()
  const side = await aside(page).last().boundingBox()
  expect(side!.x).toBeGreaterThanOrEqual(main!.x + main!.width)
  expect(Math.abs(side!.y - main!.y)).toBeLessThan(2)
})

for (const width of [1153, 1400, 1700, 390]) {
  test(`at ${width}px a container's details have room beside its meters`, async ({
    page,
  }) => {
    await openServicePage(page, width)
    const row = page.locator('li', { hasText: 'standalone' }).first()
    await expect(row).toBeVisible()
    await expect(row.getByText('CPU', { exact: true })).toBeVisible()

    const layout = await row.evaluate((item) => {
      const [identity, meters] = Array.from(
        item.firstElementChild?.children ?? []
      )
      const box = meters.getBoundingClientRect()
      return {
        overflowing: identity.scrollWidth > identity.clientWidth,
        overlapping: Array.from(identity.children).some((child) => {
          const part = child.getBoundingClientRect()
          return (
            part.left < box.right &&
            box.left < part.right &&
            part.top < box.bottom &&
            box.top < part.bottom
          )
        }),
      }
    })
    expect(layout).toEqual({ overflowing: false, overlapping: false })
  })

  test(`at ${width}px every metric tile has room for its label`, async ({
    page,
  }) => {
    await openServicePage(page, width)
    const tile = page.getByRole('button', { name: /Connected Clients/ })
    await expect(tile).toBeVisible()

    // Squeezed into six columns these were about 70px, clipping their labels.
    const box = await tile.boundingBox()
    expect(box!.width).toBeGreaterThanOrEqual(120)
  })

  test(`at ${width}px the workload modes stack and the changes fit`, async ({
    page,
  }) => {
    await openServicePage(page, width)
    const modes = await Promise.all(
      ['Legacy', 'Shadow', 'Enforce'].map((name) =>
        page.getByRole('button', { name: new RegExp(`^${name}`) }).boundingBox()
      )
    )
    for (const [above, below] of [
      [modes[0], modes[1]],
      [modes[1], modes[2]],
    ]) {
      expect(below!.y).toBeGreaterThanOrEqual(above!.y + above!.height)
      expect(below!.x).toBe(above!.x)
    }

    // A short value keeps its one word, a long list wraps in its own space.
    const changes = page.locator('li', { hasText: 'After recreate' })
    await expect(changes).toHaveCount(2)
    const value = changes.last().getByText('false', { exact: true })
    const { height, lineHeight } = await value.evaluate((element) => ({
      height: element.getBoundingClientRect().height,
      lineHeight: parseFloat(getComputedStyle(element).lineHeight),
    }))
    expect(height).toBeLessThanOrEqual(lineHeight + 1)
    const overflowing = await changes
      .first()
      .evaluate((item) => item.scrollWidth > item.clientWidth)
    expect(overflowing).toBe(false)
  })
}
