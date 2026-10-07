// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createClient, createConfig } from '@/api/client/client'
import type {
  OperationEntry,
  OperationsListResponse,
} from '@/api/client/types.gen'
import { operationsTrayFeed } from '@/lib/operations'
import { QueryClient, QueryObserver } from '@tanstack/react-query'
import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  finishedOperationsPageOptions,
  runningOperationsPageOptions,
} from './operations-queries'
import {
  getOperationsTrayState,
  setOperationsTrayOpen,
  setOperationsTrayPage,
} from './operations-tray-store'
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

    const running = await queryClient.fetchQuery(
      runningOperationsPageOptions(1, { client })
    )
    const finished = await queryClient.fetchQuery(
      finishedOperationsPageOptions(1, { client })
    )
    const feed = operationsTrayFeed({
      runningPage: running,
      finishedPage: finished,
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
            nav: feed.runningNav,
            isPending: false,
            isError: false,
            onRetry: noop,
            isPaging: false,
            onNewer: noop,
            onOlder: noop,
          }}
          recent={{
            operations: feed.recent,
            nav: feed.recentNav,
            isPending: false,
            isError: false,
            onRetry: noop,
            isPaging: false,
            onNewer: noop,
            onOlder: noop,
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

  test('older history replaces the page instead of accumulating rows', async () => {
    const { client, requests } = stubFeed(finishedOps(45))
    const queryClient = new QueryClient()

    const first = await queryClient.fetchQuery(
      finishedOperationsPageOptions(1, { client })
    )
    const second = await queryClient.fetchQuery(
      finishedOperationsPageOptions(2, { client })
    )

    expect(requests[1]?.searchParams.get('page')).toBe('2')
    expect(requests[1]?.searchParams.get('status')).toBe('finished')
    expect(requests[1]?.searchParams.get('page_size')).toBe('20')
    // Each response is one page; the tray renders only the page it is on.
    const feed = operationsTrayFeed({
      runningPage: undefined,
      finishedPage: second,
    })
    expect(feed.recent.map((op) => op.id)).toEqual(
      second.operations.map((op) => op.id)
    )
    expect(feed.recent[0]?.id).not.toBe(first.operations[0]?.id)
    expect(feed.recentNav).toMatchObject({ page: 2, first: 21, last: 40 })
  })

  test('a browsed page is dropped from the cache once nothing shows it', async () => {
    // REGRESSION (Greptile on #1295): loaded history must not outlive the
    // tray. Pages past the first use gcTime 0, so leaving them frees them.
    const { client } = stubFeed(finishedOps(45))
    const queryClient = new QueryClient()
    const options = finishedOperationsPageOptions(3, { client })
    const observer = new QueryObserver(queryClient, options)
    const unsubscribe = observer.subscribe(noop)
    await observer.refetch()
    expect(queryClient.getQueryData(options.queryKey)).toBeDefined()

    unsubscribe()
    await new Promise((resolve) => setTimeout(resolve, 5))

    expect(queryClient.getQueryData(options.queryKey)).toBeUndefined()
  })

  test('the first page keeps a cache lifetime for the badge', () => {
    expect(finishedOperationsPageOptions(1).gcTime).toBeUndefined()
    expect(runningOperationsPageOptions(1).gcTime).toBeUndefined()
    expect(finishedOperationsPageOptions(2).gcTime).toBe(0)
    expect(runningOperationsPageOptions(2).gcTime).toBe(0)
  })

  test('closing the tray returns both sections to their first page', () => {
    setOperationsTrayOpen(true)
    setOperationsTrayPage('recent', 4)
    setOperationsTrayPage('running', 2)
    expect(getOperationsTrayState().pages).toEqual({ running: 2, recent: 4 })

    setOperationsTrayOpen(false)

    expect(getOperationsTrayState().pages).toEqual({ running: 1, recent: 1 })
  })

  test('running work beyond one page of 100 is reachable on the next page', async () => {
    const running = Array.from({ length: 130 }, (_, index) =>
      entry({
        id: `deployment:${index}`,
        status: 'running',
        finished_at: null,
      })
    )
    const { client, requests } = stubFeed(running)
    const queryClient = new QueryClient()

    const first = operationsTrayFeed({
      runningPage: await queryClient.fetchQuery(
        runningOperationsPageOptions(1, { client })
      ),
      finishedPage: undefined,
    })
    expect(first.runningCount).toBe(130)
    expect(first.running).toHaveLength(100)
    expect(first.runningNav.hasOlder).toBe(true)

    const second = operationsTrayFeed({
      runningPage: await queryClient.fetchQuery(
        runningOperationsPageOptions(2, { client })
      ),
      finishedPage: undefined,
    })
    expect(requests[1]?.searchParams.get('page')).toBe('2')
    expect(requests[1]?.searchParams.get('status')).toBe('running')
    expect(second.running).toHaveLength(30)
    expect(second.runningNav).toMatchObject({ first: 101, last: 130, hasOlder: false })
  })
})
