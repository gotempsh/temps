// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  OperationEntry,
  OperationsListResponse,
} from '@/api/client/types.gen'
import { QueryClient } from '@tanstack/react-query'
import { describe, expect, test } from 'bun:test'
import {
  FINISHED_OPERATIONS_QUERY,
  flattenOperationPages,
  formatRelativeShort,
  groupOperations,
  invalidateOperations,
  isActiveOperationStatus,
  OPERATIONS_ACTIVE_POLL_MS,
  OPERATIONS_OPEN_POLL_MS,
  operationContext,
  operationsBadgeText,
  operationsLeftRunning,
  operationsNextPage,
  operationsPollInterval,
  operationsTrayFeed,
  operationStatusVariant,
  operationsTriggerLabel,
  operationTimestamp,
  RUNNING_OPERATIONS_QUERY,
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

describe('operationsNextPage', () => {
  test('asks for the next page while rows remain on the server', () => {
    expect(
      operationsNextPage(page([entry()], { page: 1, page_size: 20, total: 45 }))
    ).toBe(2)
    expect(
      operationsNextPage(page([entry()], { page: 2, page_size: 20, total: 45 }))
    ).toBe(3)
  })

  test('stops once every row is loaded', () => {
    expect(
      operationsNextPage(page([entry()], { page: 3, page_size: 20, total: 45 }))
    ).toBeUndefined()
    expect(
      operationsNextPage(page([entry()], { page: 1, page_size: 20, total: 20 }))
    ).toBeUndefined()
  })

  test('stops on an empty page even if the total says otherwise', () => {
    expect(
      operationsNextPage(page([], { page: 2, page_size: 20, total: 45 }))
    ).toBeUndefined()
  })
})

describe('flattenOperationPages', () => {
  test('concatenates pages in order and drops repeated ids', () => {
    const a = entry({ id: 'deployment:3' })
    const b = entry({ id: 'deployment:2' })
    const c = entry({ id: 'deployment:1' })
    // `b` shifted onto page 2 after a new operation arrived.
    const rows = flattenOperationPages([page([a, b]), page([b, c])])
    expect(rows.map((op) => op.id)).toEqual([
      'deployment:3',
      'deployment:2',
      'deployment:1',
    ])
  })

  test('skips excluded ids and tolerates no data', () => {
    const rows = flattenOperationPages(
      [page([entry({ id: 'a' }), entry({ id: 'b' })])],
      new Set(['a'])
    )
    expect(rows.map((op) => op.id)).toEqual(['b'])
    expect(flattenOperationPages(undefined)).toEqual([])
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
    const feed = operationsTrayFeed({
      runningPages: [page([restore], { running_count: 1, page_size: 100 })],
      finishedPages: [page(finished, { running_count: 1, total: 60 })],
    })
    expect(feed.runningCount).toBe(1)
    expect(feed.running.map((op) => op.id)).toEqual(['restore:7'])
    expect(feed.recent).toHaveLength(20)
    expect(feed.runningNotLoaded).toBe(0)
  })

  test('reports counted running work that is not loaded yet', () => {
    const feed = operationsTrayFeed({
      runningPages: [
        page([entry({ id: 'a' }), entry({ id: 'b' })], {
          running_count: 5,
          total: 5,
          page_size: 2,
        }),
      ],
      finishedPages: undefined,
    })
    expect(feed.runningCount).toBe(5)
    expect(feed.runningNotLoaded).toBe(3)
  })

  test('a row in both feeds is shown once, under running', () => {
    const feed = operationsTrayFeed({
      runningPages: [page([entry({ id: 'x' })], { running_count: 1 })],
      finishedPages: [
        page([entry({ id: 'x', status: 'succeeded' }), entry({ id: 'y' })]),
      ],
    })
    expect(feed.running.map((op) => op.id)).toEqual(['x'])
    expect(feed.recent.map((op) => op.id)).toEqual(['y'])
  })

  test('nothing loaded yet means nothing counted', () => {
    const feed = operationsTrayFeed({
      runningPages: undefined,
      finishedPages: undefined,
    })
    expect(feed).toEqual({
      running: [],
      recent: [],
      runningCount: 0,
      runningNotLoaded: 0,
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
