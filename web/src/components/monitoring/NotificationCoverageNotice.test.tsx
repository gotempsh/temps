// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { NotificationDeliveryCoverageResponse } from '@/api/client/types.gen'
import { CoverageMessage } from './NotificationCoverageNotice'

function render(coverage: NotificationDeliveryCoverageResponse) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <CoverageMessage
        coverage={coverage}
        returnTo="/projects/demo/errors/alert-rules/new"
      />
    </MemoryRouter>
  )
}

const base: NotificationDeliveryCoverageResponse = {
  severity: 'error',
  configured: false,
  route_ids: [],
  provider_ids: [],
  cloud_delivery: false,
  reason: 'No notification provider is configured',
  setup_path: '/settings/notifications/new',
}

function hrefs(html: string): URL[] {
  return [...html.matchAll(/href="([^"]+)"/g)].map(
    (match) => new URL(match[1].replace(/&amp;/g, '&'), 'https://temps.invalid')
  )
}

describe('CoverageMessage', () => {
  test('warns and links to add a provider that returns to the form', () => {
    const html = render(base)
    expect(html).toContain('This rule won&#x27;t notify anyone yet')
    expect(html).toContain('No notification provider is configured')
    const [link] = hrefs(html)
    expect(link.pathname).toBe('/settings/notifications/new')
    expect(link.searchParams.get('returnTo')).toBe(
      '/projects/demo/errors/alert-rules/new'
    )
  })

  test('points at routes when providers exist but none match', () => {
    const html = render({
      ...base,
      reason:
        'No enabled notification route sends error notifications to an enabled provider',
      setup_path: '/settings/notifications?tab=routes',
    })
    expect(html).toContain('Review routes')
    const [link] = hrefs(html)
    expect(link.searchParams.get('tab')).toBe('routes')
    expect(link.searchParams.get('returnTo')).toBe(
      '/projects/demo/errors/alert-rules/new'
    )
  })

  test('confirms delivery and states the all-projects scope', () => {
    const html = render({
      ...base,
      configured: true,
      route_ids: [1],
      provider_ids: [2, 3],
      reason: null,
      setup_path: null,
    })
    expect(html).not.toContain('notify anyone yet')
    expect(html).toContain('Error notifications go to 2 providers')
    expect(html).toContain('(all projects)')
  })
})
