// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { OperationEntry } from '@/api/client/types.gen'
import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  OPERATIONS_EMPTY_MESSAGE,
  OperationsTrayPanel,
  type OperationsSectionState,
  type OperationsTrayPanelProps,
} from './OperationsTrayPanel'
import {
  EMPTY_OPERATIONS_PAGE_NAV,
  type OperationsPageNav,
} from '@/lib/operations'

const NOW = Date.parse('2026-10-07T12:00:00Z')
const noop = () => {}

function entry(overrides: Partial<OperationEntry> = {}): OperationEntry {
  return {
    id: 'deployment:42',
    kind: 'rollback',
    status: 'running',
    title: 'Rollback to deployment #41',
    project_id: 3,
    project_slug: 'shop',
    environment_id: 5,
    environment_name: 'production',
    deployment_id: 42,
    service_id: null,
    service_name: null,
    backup_id: null,
    restore_run_id: null,
    agent_run_id: null,
    phase: null,
    failure_reason: null,
    created_at: '2026-10-07T11:55:00Z',
    started_at: '2026-10-07T11:56:00Z',
    finished_at: null,
    triggered_by_user_id: null,
    link: '/projects/shop/deployments/42',
    ...overrides,
  }
}

function section(
  overrides: Partial<OperationsSectionState> = {}
): OperationsSectionState {
  return {
    operations: [],
    isPending: false,
    isError: false,
    errorMessage: null,
    onRetry: noop,
    nav: EMPTY_OPERATIONS_PAGE_NAV,
    isPaging: false,
    onNewer: noop,
    onOlder: noop,
    ...overrides,
  }
}

function nav(overrides: Partial<OperationsPageNav>): OperationsPageNav {
  return { ...EMPTY_OPERATIONS_PAGE_NAV, ...overrides }
}

function render(props: Partial<OperationsTrayPanelProps> = {}) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <OperationsTrayPanel
        running={section()}
        recent={section()}
        localOperations={[]}
        runningCount={0}
        onNavigate={noop}
        now={NOW}
        {...props}
      />
    </MemoryRouter>
  )
}

describe('OperationsTrayPanel', () => {
  test('loading renders skeleton rows, not a spinner', () => {
    const html = render({
      running: section({ isPending: true }),
      recent: section({ isPending: true }),
    })
    expect(html).toContain('animate-pulse')
    expect(html).not.toContain('animate-spin')
  })

  test('error state explains and offers retry', () => {
    const html = render({
      running: section({
        isError: true,
        errorMessage: 'Database timeout',
      }),
      recent: section({ isError: true, errorMessage: 'Database timeout' }),
    })
    expect(html).toContain('load operations')
    expect(html).toContain('Database timeout')
    expect(html).toContain('Retry')
  })

  test('empty state says what will appear', () => {
    expect(render()).toContain(OPERATIONS_EMPTY_MESSAGE)
  })

  test('rows link to the resource with status, context and time', () => {
    const html = render({
      running: section({ operations: [entry()] }),
      runningCount: 1,
    })
    expect(html).toContain('href="/projects/shop/deployments/42"')
    expect(html).toContain('Rollback to deployment #41')
    expect(html).toContain('Running')
    expect(html).toContain('shop · production')
    expect(html).toContain('4m ago')
    expect(html).toContain('1 running')
  })

  test('failed rows show the failure reason', () => {
    const html = render({
      recent: section({
        operations: [
          entry({
            id: 'backup:3',
            kind: 'backup',
            status: 'failed',
            title: 'Backup of orders-db',
            failure_reason: 'bucket unreachable',
            finished_at: '2026-10-07T11:00:00Z',
            link: '/backups/s3-sources/2/backups/b-uuid',
          }),
        ],
      }),
    })
    expect(html).toContain('Failed')
    expect(html).toContain('bucket unreachable')
    expect(html).toContain('1h ago')
  })

  test('renders running and recent work as separate sections', () => {
    const html = render({
      running: section({ operations: [entry()] }),
      recent: section({
        operations: [
          entry({ id: 'deployment:40', status: 'succeeded', title: 'Deploy' }),
        ],
      }),
      runningCount: 1,
    })
    expect(html).toContain('aria-label="Running"')
    expect(html).toContain('aria-label="Recent"')
    expect(html.indexOf('aria-label="Running"')).toBeLessThan(
      html.indexOf('aria-label="Recent"')
    )
  })

  test('an old running operation still renders behind 20 newer finished ones', () => {
    const oldRestore = entry({
      id: 'restore:7',
      kind: 'restore',
      title: 'Restore orders-db',
      created_at: '2026-10-06T08:00:00Z',
      started_at: '2026-10-06T08:00:05Z',
      link: '/storage/7/restores/7',
    })
    const newerFinished = Array.from({ length: 20 }, (_, index) =>
      entry({
        id: `deployment:${100 + index}`,
        kind: 'deployment',
        status: 'succeeded',
        title: `Deploy #${100 + index}`,
        created_at: '2026-10-07T11:00:00Z',
        finished_at: '2026-10-07T11:01:00Z',
      })
    )
    const html = render({
      running: section({ operations: [oldRestore] }),
      recent: section({
        operations: newerFinished,
        nav: nav({ hasOlder: true, first: 1, last: 20, total: 60 }),
      }),
      runningCount: 1,
    })
    expect(html).toContain('Restore orders-db')
    expect(html).toContain('href="/storage/7/restores/7"')
    expect(html).toContain('Deploy #119')
    expect(html).toContain('1 running')
  })

  test('counted running work past the first page is one click away', () => {
    const html = render({
      running: section({
        operations: [entry()],
        nav: nav({ page: 1, hasOlder: true, first: 1, last: 100, total: 113 }),
      }),
      runningCount: 113,
    })
    expect(html).toContain('aria-label="Older running operations"')
    expect(html).toContain('1–100 of 113')
    expect(html).toContain('113 running')
  })

  test('history pages replace each other and disable while loading', () => {
    const finished = section({
      operations: [entry({ id: 'deployment:40', status: 'succeeded' })],
      nav: nav({ page: 2, hasNewer: true, hasOlder: true, first: 21, last: 40, total: 60 }),
    })
    const html = render({ recent: finished })
    expect(html).toContain('aria-label="Newer recent operations"')
    expect(html).toContain('aria-label="Older recent operations"')
    expect(html).toContain('21–40 of 60')
    const paging = render({ recent: { ...finished, isPaging: true } })
    expect(paging).toContain('Loading…')
    expect(paging).toContain('disabled')
  })

  test('no paging controls when everything fits on one page', () => {
    const html = render({
      recent: section({
        operations: [entry({ id: 'deployment:40', status: 'succeeded' })],
        nav: nav({ first: 1, last: 1, total: 1 }),
      }),
    })
    expect(html).not.toContain('Older recent operations')
    expect(html).not.toContain('Newer recent operations')
  })

  test('a failing history feed keeps running rows and offers retry', () => {
    const html = render({
      running: section({ operations: [entry()] }),
      recent: section({ isError: true, errorMessage: 'history timed out' }),
      runningCount: 1,
    })
    expect(html).toContain('Rollback to deployment #41')
    expect(html).toContain('load recent operations')
    expect(html).toContain('history timed out')
    expect(html).toContain('Retry')
  })

  test('history still loading shows a skeleton under the running rows', () => {
    const html = render({
      running: section({ operations: [entry()] }),
      recent: section({ isPending: true }),
      runningCount: 1,
    })
    expect(html).toContain('Rollback to deployment #41')
    expect(html).toContain('animate-pulse')
    expect(html).not.toContain(OPERATIONS_EMPTY_MESSAGE)
  })

  test('local entries are listed first and marked as not persisted', () => {
    const html = render({
      localOperations: [
        {
          id: 'local:1',
          title: 'Restarting container',
          context: 'abc123def456',
          startedAt: NOW,
        },
      ],
    })
    expect(html).toContain('Restarting container')
    expect(html).toContain('not kept after a refresh')
    expect(html).not.toContain(OPERATIONS_EMPTY_MESSAGE)
    expect(html).toContain('1 running')
  })
})
