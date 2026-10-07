// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createClient, createConfig } from '@/api/client/client'
import type {
  OperationEntry,
  OperationsListResponse,
} from '@/api/client/types.gen'
import { operationsTrayFeed } from '@/lib/operations'
import { InfiniteQueryObserver, QueryClient } from '@tanstack/react-query'
import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  finishedOperationsInfiniteOptions,
  runningOperationsInfiniteOptions,
} from './operations-queries'
import { OperationsTrayPanel } from './OperationsTrayPanel'

const NOW = Date.parse('2026-10-07T12:00:00Z')
const noop = () => {}

function entry(overrides: Partial<OperationEntry> = {}): OperationEntry {
  return {
    id: 'deployment:1',
    kind: 'deployment',
    status: 'succeeded',
    title: 'Deploy',
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
    created_at: '2026-10-07T11:00:00Z',
    started_at: '2026-10-07T11:00:05Z',
    finished_at: '2026-10-07T11:01:00Z',
    triggered_by_user_id: null,
    link: '/projects/shop/deployments/1',
    ...overrides,
  }
}

/** Serves `operations` paged and filtered like `GET /operations`. */
function stubFeed(operations: readonly OperationEntry[]) {
  const requests: URL[] = []
  const serve = async (input: RequestInfo | URL): Promise<Response> => {
    const url = new URL(input instanceof Request ? input.url : String(input))
    requests.push(url)
    const status = url.searchParams.get('status') ?? 'all'
    const page = Number(url.searchParams.get('page') ?? '1')
    const pageSize = Number(url.searchParams.get('page_size') ?? '20')
    const isRunning = (op: OperationEntry) =>
      ['queued', 'running', 'waiting'].includes(op.status)
    const matching = operations.filter((op) =>
      status === 'running'
        ? isRunning(op)
        : status === 'finished'
          ? !isRunning(op)
          : true
    )
    const body: OperationsListResponse = {
      operations: matching.slice((page - 1) * pageSize, page * pageSize),
      page,
      page_size: pageSize,
      running_count: operations.filter(isRunning).length,
      total: matching.length,
    }
    return new Response(JSON.stringify(body), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  }
  const client = createClient(
    createConfig({
      baseUrl: 'http://temps.test/api',
      // Bun's `typeof fetch` adds a `preconnect` member the client never uses.
      fetch: Object.assign(serve, { preconnect: () => {} }),
    })
  )
  return { client, requests }
}

function finishedOps(count: number, startId = 100): OperationEntry[] {
  return Array.from({ length: count }, (_, index) =>
    entry({
      id: `deployment:${startId + index}`,
      title: `Deploy #${startId + index}`,
    })
  )
}

describe('operations tray queries', () => {
  test('a running operation older than 20 newer finished ones still renders', async () => {
    const oldRestore = entry({
      id: 'restore:7',
      kind: 'restore',
      status: 'running',
      title: 'Restore orders-db',
      created_at: '2026-10-06T08:00:00Z',
      started_at: '2026-10-06T08:00:05Z',
      finished_at: null,
      link: '/storage/7/restores/7',
    })
    // Newest first, as the API orders them: the restore is the 21st row of
    // the unfiltered feed, past the old single 20-row page.
    const { client, requests } = stubFeed([...finishedOps(20), oldRestore])
    const queryClient = new QueryClient()

    const running = await queryClient.fetchInfiniteQuery(
      runningOperationsInfiniteOptions({ client })
    )
    const finished = await queryClient.fetchInfiniteQuery(
      finishedOperationsInfiniteOptions({ client })
    )
    const feed = operationsTrayFeed({
      runningPages: running.pages,
      finishedPages: finished.pages,
    })

    expect(requests[0]?.searchParams.get('status')).toBe('running')
    expect(requests[0]?.searchParams.get('page_size')).toBe('100')
    expect(requests[1]?.searchParams.get('status')).toBe('finished')
    expect(requests[1]?.searchParams.get('page_size')).toBe('20')
    expect(feed.runningCount).toBe(1)
    expect(feed.running.map((op) => op.id)).toEqual(['restore:7'])
    expect(feed.recent).toHaveLength(20)

    const html = renderToStaticMarkup(
      <MemoryRouter>
        <OperationsTrayPanel
          running={{
            operations: feed.running,
            notLoaded: feed.runningNotLoaded,
            isPending: false,
            isError: false,
            onRetry: noop,
            hasMore: false,
            isFetchingMore: false,
            onLoadMore: noop,
          }}
          recent={{
            operations: feed.recent,
            isPending: false,
            isError: false,
            onRetry: noop,
            hasMore: false,
            isFetchingMore: false,
            onLoadMore: noop,
          }}
          localOperations={[]}
          runningCount={feed.runningCount}
          onNavigate={noop}
          now={NOW}
        />
      </MemoryRouter>
    )
    expect(html).toContain('Restore orders-db')
    expect(html).toContain('href="/storage/7/restores/7"')
  })

  test('load more fetches the next page of finished history', async () => {
    const { client, requests } = stubFeed(finishedOps(25))
    const queryClient = new QueryClient()
    const options = finishedOperationsInfiniteOptions({ client })

    await queryClient.fetchInfiniteQuery(options)
    const observer = new InfiniteQueryObserver(queryClient, options)
    expect(observer.getCurrentResult().hasNextPage).toBe(true)

    const result = await observer.fetchNextPage()

    expect(requests).toHaveLength(2)
    expect(requests[1]?.searchParams.get('page')).toBe('2')
    expect(requests[1]?.searchParams.get('status')).toBe('finished')
    expect(requests[1]?.searchParams.get('page_size')).toBe('20')
    expect(result.data?.pages.map((page) => page.operations.length)).toEqual([
      20, 5,
    ])
    expect(result.hasNextPage).toBe(false)
  })

  test('running work beyond one page of 100 can be paged in', async () => {
    const running = Array.from({ length: 130 }, (_, index) =>
      entry({
        id: `deployment:${index}`,
        status: 'running',
        finished_at: null,
      })
    )
    const { client, requests } = stubFeed(running)
    const queryClient = new QueryClient()
    const options = runningOperationsInfiniteOptions({ client })

    const first = await queryClient.fetchInfiniteQuery(options)
    const partial = operationsTrayFeed({
      runningPages: first.pages,
      finishedPages: undefined,
    })
    expect(partial.runningCount).toBe(130)
    expect(partial.running).toHaveLength(100)
    expect(partial.runningNotLoaded).toBe(30)

    const observer = new InfiniteQueryObserver(queryClient, options)
    const result = await observer.fetchNextPage()

    expect(requests[1]?.searchParams.get('page')).toBe('2')
    expect(requests[1]?.searchParams.get('status')).toBe('running')
    const complete = operationsTrayFeed({
      runningPages: result.data?.pages,
      finishedPages: undefined,
    })
    expect(complete.running).toHaveLength(130)
    expect(complete.runningNotLoaded).toBe(0)
    expect(result.hasNextPage).toBe(false)
  })
})
