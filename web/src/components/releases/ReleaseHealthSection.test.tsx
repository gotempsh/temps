// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type { ReleaseHealth, ReleaseHealthPage } from '@/api/client'
import {
  ReleaseHealthContent,
  ReleaseHealthSeriesContent,
} from './ReleaseHealthSection'

const release = (overrides: Partial<ReleaseHealth>): ReleaseHealth => ({
  release: 'synthetic-app@1.0.0',
  sessions: 100,
  errored_sessions: 8,
  crashed_sessions: 5,
  abnormal_sessions: 0,
  users: 80,
  crashed_users: 4,
  crash_free_sessions: 0.95,
  crash_free_users: 0.95,
  adoption: 0.6,
  sessions_last_24h: 60,
  first_session_hour: '2026-11-19T10:00:00Z',
  last_session_hour: '2026-11-20T10:00:00Z',
  released_at: null,
  deployment: null,
  ...overrides,
})

const page = (overrides: Partial<ReleaseHealthPage>): ReleaseHealthPage => ({
  items: [],
  total: 0,
  page: 1,
  per_page: 20,
  environments: [],
  start_time: '2026-11-13T12:00:00Z',
  end_time: '2026-11-20T12:00:00Z',
  tracking: { configured: true, reason: null, setup_path: null },
  ...overrides,
})

const render = (data: ReleaseHealthPage) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <ReleaseHealthContent data={data} projectSlug="shop" onOpen={() => {}} />
    </MemoryRouter>
  )

describe('ReleaseHealthContent', () => {
  test('without sessions it says what is missing and links to the SDK setup', () => {
    const html = render(
      page({
        tracking: {
          configured: false,
          reason: 'No sessions received yet.',
          setup_path: 'errors/setup',
        },
      })
    )
    expect(html).toContain('No sessions received yet.')
    expect(html).toContain('href="/projects/shop/errors/setup"')
    expect(html).toContain('Sentry.init')
  })

  test('lists rates, adoption and the deployment that shipped a release', () => {
    const html = render(
      page({
        total: 3,
        environments: ['production'],
        items: [
          release({
            release: '0123456789abcdef0123456789abcdef01234567',
            deployment: {
              deployment_id: 42,
              environment_id: 3,
              environment_name: 'production',
              ready_at: '2026-11-20T09:00:00Z',
              commit_message: 'Synthetic change',
            },
          }),
          release({ release: 'synthetic-old', crash_free_sessions: 1 }),
        ],
      })
    )
    expect(html).toContain('0123456789ab')
    expect(html).toContain('95.0%')
    expect(html).toContain('60.0%')
    expect(html).toContain('href="/projects/shop/deployments/42"')
    expect(html).toContain('Deployed to production')
    expect(html).toContain('Not deployed by Temps')
    expect(html).toContain('5 crashed')
    expect(html).toContain('The 2 most recently active of 3')
  })

  test('fits narrow containers: secondary columns hide and their values move under the release', () => {
    const html = render(
      page({
        total: 1,
        items: [
          release({
            crash_free_users: 0.9,
            adoption: 0.25,
            sessions: 1234,
            users: 321,
          }),
        ],
      })
    )
    expect(html).not.toContain('overflow-x-auto')
    expect(html).toContain('@container')
    for (const breakpoint of ['@xl', '@2xl', '@3xl']) {
      expect(html).toContain(`hidden ${breakpoint}:table-cell`)
    }
    const summary = html.slice(html.indexOf('@3xl:hidden'))
    expect(summary).toContain('90.0% crash-free users')
    expect(summary).toContain('25.0% adoption')
    expect(summary).toContain('1,234 sessions · ~321 users')
  })

  test('an empty window says so', () => {
    expect(render(page({}))).toContain('No session started in this window')
  })
})

describe('ReleaseHealthSeriesContent', () => {
  test('a release without sessions in the window says so', () => {
    const html = renderToStaticMarkup(
      <ReleaseHealthSeriesContent
        series={{
          release: 'synthetic-app@1.0.0',
          environment: null,
          points: [],
          start_time: '2026-11-13T12:00:00Z',
          end_time: '2026-11-20T12:00:00Z',
        }}
      />
    )
    expect(html).toContain('No session of this release started')
  })

  test('totals the sessions and crashes of the window', () => {
    const html = renderToStaticMarkup(
      <ReleaseHealthSeriesContent
        series={{
          release: 'synthetic-app@1.0.0',
          environment: 'production',
          points: [
            {
              bucket_start: '2026-11-20T10:00:00Z',
              sessions: 60,
              errored_sessions: 4,
              crashed_sessions: 3,
              abnormal_sessions: 1,
              crash_free_sessions: 0.95,
            },
            {
              bucket_start: '2026-11-20T11:00:00Z',
              sessions: 40,
              errored_sessions: 0,
              crashed_sessions: 0,
              abnormal_sessions: 0,
              crash_free_sessions: 1,
            },
          ],
          start_time: '2026-11-13T12:00:00Z',
          end_time: '2026-11-20T12:00:00Z',
        }}
      />
    )
    expect(html).toContain('100 sessions')
    expect(html).toContain('3 crashed')
    expect(html).toContain('1 abnormal')
  })
})
