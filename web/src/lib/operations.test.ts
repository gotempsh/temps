// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  OperationEntry,
  OperationsListResponse,
} from '@/api/client/types.gen'
import { QueryClient } from '@tanstack/react-query'
import { describe, expect, test } from 'bun:test'
import {
  EMPTY_OPERATIONS_PAGE_NAV,
  FINISHED_OPERATIONS_QUERY,
  formatRelativeShort,
  groupOperations,
  invalidateOperations,
  isActiveOperationStatus,
  OPERATIONS_ACTIVE_POLL_MS,
  OPERATIONS_OPEN_POLL_MS,
  operationContext,
  operationsBadgeText,
  operationsClampPage,
  operationsLastPage,
  operationsLeftRunning,
  operationsPageNav,
  operationsPollInterval,
  operationsTrayFeed,
  operationStatusVariant,
  operationsTriggerLabel,
  operationTimestamp,
  RUNNING_OPERATIONS_QUERY,
  uniqueOperations,
} from './operations'

function entry(overrides: Partial<OperationEntry> = {}): OperationEntry {
  return {
    id: 'deployment:1',
    kind: 'deployment',
    status: 'running',
    title: 'Deploy main @ 0123456',
    project_id: 3,
    project_slug: 'shop',
    environment_id: 5,
    environment_name: 'production',
    deployment_id: 1,
    service_id: null,
    service_name: null,
    backup_id: null,
    restore_run_id: null,
    agent_run_id: null,
    phase: null,
    failure_reason: null,
    created_at: '2026-10-07T10:00:00Z',
    started_at: '2026-10-07T10:00:05Z',
    finished_at: null,
    triggered_by_user_id: null,
    link: '/projects/shop/deployments/1',
    ...overrides,
  }
}

describe('operationsPollInterval', () => {
  test('polls fast while the tray is open', () => {
    expect(operationsPollInterval({ open: true, runningCount: 0 })).toBe(
      OPERATIONS_OPEN_POLL_MS
    )
  })

  test('polls slowly while something is running', () => {
    expect(operationsPollInterval({ open: false, runningCount: 2 })).toBe(
      OPERATIONS_ACTIVE_POLL_MS
    )
    expect(
      operationsPollInterval({ open: false, runningCount: 0, localCount: 1 })
    ).toBe(OPERATIONS_ACTIVE_POLL_MS)
  })

  test('does not poll when idle and closed', () => {
    expect(operationsPollInterval({ open: false, runningCount: 0 })).toBe(false)
  })
})

describe('status helpers', () => {
  test('active statuses are queued, running and waiting', () => {
    expect(isActiveOperationStatus('queued')).toBe(true)
    expect(isActiveOperationStatus('running')).toBe(true)
    expect(isActiveOperationStatus('waiting')).toBe(true)
    expect(isActiveOperationStatus('succeeded')).toBe(false)
    expect(isActiveOperationStatus('failed')).toBe(false)
    expect(isActiveOperationStatus('cancelled')).toBe(false)
  })

  test('variants follow the outcome', () => {
    expect(operationStatusVariant('succeeded')).toBe('success')
    expect(operationStatusVariant('failed')).toBe('destructive')
    expect(operationStatusVariant('waiting')).toBe('warning')
    expect(operationStatusVariant('running')).toBe('secondary')
  })
})

describe('row helpers', () => {
  test('context names project and environment, or the service', () => {
    expect(operationContext(entry())).toBe('shop · production')
    expect(
      operationContext(
        entry({
          kind: 'backup',
          project_slug: null,
          environment_name: null,
          service_name: 'orders-db',
        })
      )
    ).toBe('orders-db')
    expect(
      operationContext(
        entry({
          kind: 'restore',
          environment_name: null,
          service_name: 'orders-db',
        })
      )
    ).toBe('shop · orders-db')
  })

  test('timestamp is the finish time for finished work', () => {
    expect(operationTimestamp(entry())).toBe('2026-10-07T10:00:05Z')
    expect(
      operationTimestamp(
        entry({ status: 'failed', finished_at: '2026-10-07T10:03:00Z' })
      )
    ).toBe('2026-10-07T10:03:00Z')
    expect(operationTimestamp(entry({ started_at: null }))).toBe(
      '2026-10-07T10:00:00Z'
    )
  })

  test('relative time is compact', () => {
    const now = Date.parse('2026-10-07T12:00:00Z')
    expect(formatRelativeShort('2026-10-07T11:59:50Z', now)).toBe('just now')
    expect(formatRelativeShort('2026-10-07T11:56:00Z', now)).toBe('4m ago')
    expect(formatRelativeShort('2026-10-07T09:00:00Z', now)).toBe('3h ago')
    expect(formatRelativeShort('2026-10-05T12:00:00Z', now)).toBe('2d ago')
    expect(formatRelativeShort('not a date', now)).toBe('')
  })

  test('grouping splits active from finished and keeps order', () => {
    const groups = groupOperations([
      entry({ id: 'a', status: 'running' }),
      entry({ id: 'b', status: 'failed' }),
      entry({ id: 'c', status: 'waiting' }),
      entry({ id: 'd', status: 'succeeded' }),
    ])
    expect(groups.active.map((op) => op.id)).toEqual(['a', 'c'])
    expect(groups.finished.map((op) => op.id)).toEqual(['b', 'd'])
  })
})

describe('trigger labels', () => {
  test('badge hides at zero and caps at 9+', () => {
    expect(operationsBadgeText(0)).toBeNull()
    expect(operationsBadgeText(3)).toBe('3')
    expect(operationsBadgeText(12)).toBe('9+')
  })

  test('accessible label always says Operations', () => {
    expect(operationsTriggerLabel(0)).toBe('Operations')
    expect(operationsTriggerLabel(2)).toBe('Operations (2 running)')
  })
})

describe('invalidateOperations', () => {
  test('invalidates every operations query regardless of params', async () => {
    const queryClient = new QueryClient()
    const operationsKey = [
      { _id: 'listOperations', baseUrl: '/api', query: { page: 1 } },
    ]
    const otherKey = [{ _id: 'listBackupAlerts', baseUrl: '/api' }]
    queryClient.setQueryData(operationsKey, { operations: [] })
    queryClient.setQueryData(otherKey, { alerts: [] })

    await invalidateOperations(queryClient)

    expect(queryClient.getQueryState(operationsKey)?.isInvalidated).toBe(true)
    expect(queryClient.getQueryState(otherKey)?.isInvalidated).toBe(false)
  })
})

function page(
  operations: OperationEntry[],
  overrides: Partial<OperationsListResponse> = {}
): OperationsListResponse {
  return {
    operations,
    page: 1,
    page_size: 20,
    running_count: 0,
    total: operations.length,
    ...overrides,
  }
}

describe('tray queries', () => {
  test('running work is fetched apart from history, at the API maximum', () => {
    expect(RUNNING_OPERATIONS_QUERY).toEqual({
      status: 'running',
      page_size: 100,
    })
    expect(FINISHED_OPERATIONS_QUERY).toEqual({
      status: 'finished',
      page_size: 20,
    })
  })
})

describe('operationsPageNav', () => {
  test('locates a page within the feed', () => {
    const rows = Array.from({ length: 20 }, (_, i) => entry({ id: `d:${i}` }))
    expect(operationsPageNav(page(rows, { page: 1, total: 45 }))).toEqual({
      page: 1,
      hasNewer: false,
      hasOlder: true,
      first: 1,
      last: 20,
      total: 45,
    })
    const tail = rows.slice(0, 5)
    expect(operationsPageNav(page(tail, { page: 3, total: 45 }))).toEqual({
      page: 3,
      hasNewer: true,
      hasOlder: false,
      first: 41,
      last: 45,
      total: 45,
    })
  })

  test('an empty page has no older page even if the total says otherwise', () => {
    const nav = operationsPageNav(page([], { page: 2, total: 45 }))
    expect(nav.hasOlder).toBe(false)
    expect(nav.first).toBe(0)
  })

  test('no response yet is an empty first page', () => {
    expect(operationsPageNav(undefined)).toEqual(EMPTY_OPERATIONS_PAGE_NAV)
  })
})

describe('operationsLastPage / operationsClampPage', () => {
  test('last page holds the remainder', () => {
    expect(operationsLastPage(45, 20)).toBe(3)
    expect(operationsLastPage(40, 20)).toBe(2)
    expect(operationsLastPage(0, 20)).toBe(1)
  })

  test('steps back when the feed shrank under the user', () => {
    // Running work finished while the user sat on page 3 of 3.
    expect(operationsClampPage(3, page([], { page: 3, total: 30 }))).toBe(2)
    expect(operationsClampPage(3, page([entry()], { page: 3, total: 41 }))).toBe(
      3
    )
    expect(operationsClampPage(1, undefined)).toBe(1)
  })
})

describe('uniqueOperations', () => {
  test('drops repeated and excluded ids, keeping order', () => {
    const rows = uniqueOperations(
      [entry({ id: 'a' }), entry({ id: 'b' }), entry({ id: 'a' })],
      new Set(['b'])
    )
    expect(rows.map((op) => op.id)).toEqual(['a'])
    expect(uniqueOperations(undefined)).toEqual([])
  })
})

describe('operationsTrayFeed', () => {
  test('badge count and running rows come from the running feed', () => {
    const restore = entry({
      id: 'restore:7',
      kind: 'restore',
      created_at: '2026-10-01T00:00:00Z',
    })
    const finished = Array.from({ length: 20 }, (_, index) =>
      entry({ id: `deployment:${index}`, status: 'succeeded' })
    )
    // The restore is older than 20 newer finished operations and still
    // renders: running work never competes with history for a page.
    const feed = operationsTrayFeed({
      runningPage: page([restore], { running_count: 1, page_size: 100 }),
      finishedPage: page(finished, { running_count: 1, total: 60 }),
    })
    expect(feed.runningCount).toBe(1)
    expect(feed.running.map((op) => op.id)).toEqual(['restore:7'])
    expect(feed.recent).toHaveLength(20)
    expect(feed.recentNav.hasOlder).toBe(true)
  })

  test('holds exactly one page per section, never an accumulation', () => {
    // REGRESSION (Greptile on #1295): history used to be an infinite query
    // that kept every page loaded. Paging now replaces the rows, so the feed
    // never holds more than one page of each section.
    const older = Array.from({ length: 20 }, (_, i) =>
      entry({ id: `old:${i}`, status: 'succeeded' })
    )
    const feed = operationsTrayFeed({
      runningPage: page([], { page_size: 100 }),
      finishedPage: page(older, { page: 3, total: 300 }),
    })
    expect(feed.recent).toHaveLength(20)
    expect(feed.recent[0].id).toBe('old:0')
    expect(feed.recentNav).toMatchObject({ page: 3, first: 41, last: 60 })
  })

  test('a row in both feeds is shown once, under running', () => {
    const feed = operationsTrayFeed({
      runningPage: page([entry({ id: 'x' })], { running_count: 1 }),
      finishedPage: page([
        entry({ id: 'x', status: 'succeeded' }),
        entry({ id: 'y' }),
      ]),
    })
    expect(feed.running.map((op) => op.id)).toEqual(['x'])
    expect(feed.recent.map((op) => op.id)).toEqual(['y'])
  })

  test('nothing loaded yet means nothing counted', () => {
    expect(
      operationsTrayFeed({ runningPage: undefined, finishedPage: undefined })
    ).toEqual({
      running: [],
      recent: [],
      runningCount: 0,
      runningNav: EMPTY_OPERATIONS_PAGE_NAV,
      recentNav: EMPTY_OPERATIONS_PAGE_NAV,
    })
  })
})

describe('operationsLeftRunning', () => {
  test('detects an operation dropping out of the running feed', () => {
    expect(operationsLeftRunning(['a', 'b'], ['b'])).toBe(true)
    expect(operationsLeftRunning(['a'], ['c'])).toBe(true)
  })

  test('ignores additions, no change, and the first response', () => {
    expect(operationsLeftRunning(['a'], ['a', 'b'])).toBe(false)
    expect(operationsLeftRunning(['a', 'b'], ['b', 'a'])).toBe(false)
    expect(operationsLeftRunning([], ['a'])).toBe(false)
  })
})
