// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Header operations tray: one place to follow deployments, rollbacks,
 * promotions, restores, backups and autofix runs, backed by `GET /operations`
 * so it survives a refresh.
 *
 * In-flight work and finished history are two separately paged queries. The
 * badge count and the "Running" rows both come from the `status=running`
 * feed, so every operation the badge counts can be opened from the tray no
 * matter how much newer finished work exists. Each section shows one page at
 * a time (Newer/Older replace it) and closing the tray returns both to page
 * 1, so browsing history never accumulates rows -- see `operations-queries`
 * for the memory bound.
 *
 * Polling is on only when it is useful: every 5s while the popover is open,
 * every 10s while something is running, otherwise not at all (one fetch on
 * mount plus window-focus refetches).
 */
import { problemDetail } from '@/lib/api-problem'
import {
  operationsBadgeText,
  operationsClampPage,
  operationsLeftRunning,
  operationsPollInterval,
  operationsTrayFeed,
  operationsTriggerLabel,
} from '@/lib/operations'
import { useQuery } from '@tanstack/react-query'
import { Activity } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { Button } from '../ui/button'
import { Popover, PopoverContent, PopoverTrigger } from '../ui/popover'
import {
  finishedOperationsPageOptions,
  runningOperationsPageOptions,
} from './operations-queries'
import { OperationsTrayPanel } from './OperationsTrayPanel'
import {
  setOperationsTrayOpen,
  setOperationsTrayPage,
  useLocalOperations,
  useOperationsTrayOpen,
  useOperationsTrayPage,
} from './operations-tray-store'

export function OperationsTray() {
  const open = useOperationsTrayOpen()
  const localOperations = useLocalOperations()
  const runningPage = useOperationsTrayPage('running')
  const recentPage = useOperationsTrayPage('recent')
  const [now, setNow] = useState(() => Date.now())

  const localCount = localOperations.length

  const runningQuery = useQuery({
    ...runningOperationsPageOptions(runningPage),
    refetchInterval: (q) =>
      operationsPollInterval({
        open,
        runningCount: q.state.data?.running_count ?? 0,
        localCount,
      }),
    refetchOnWindowFocus: true,
  })
  const serverRunningCount = runningQuery.data?.running_count ?? 0

  // History follows the same policy, keyed on the running feed's count, so a
  // row that finishes while the tray is closed is already in "Recent" when
  // it opens.
  const finishedQuery = useQuery({
    ...finishedOperationsPageOptions(recentPage),
    refetchInterval: () =>
      operationsPollInterval({
        open,
        runningCount: serverRunningCount,
        localCount,
      }),
    refetchOnWindowFocus: true,
  })

  const feed = useMemo(
    () =>
      operationsTrayFeed({
        runningPage: runningQuery.data,
        finishedPage: finishedQuery.data,
      }),
    [runningQuery.data, finishedQuery.data]
  )

  // If a feed shrank under the user (work finished while they were on its
  // last page), step back to the last page that still has rows.
  const runningClamp = operationsClampPage(runningPage, runningQuery.data)
  const recentClamp = operationsClampPage(recentPage, finishedQuery.data)
  useEffect(() => {
    if (runningClamp !== runningPage) {
      setOperationsTrayPage('running', runningClamp)
    }
  }, [runningClamp, runningPage])
  useEffect(() => {
    if (recentClamp !== recentPage) setOperationsTrayPage('recent', recentClamp)
  }, [recentClamp, recentPage])

  // When something leaves the running feed, pull history right away so the
  // finished row is there without waiting for a poll (which may now be off).
  // Only consecutive responses for the same page are compared, so paging
  // through running work does not look like work finishing.
  const { refetch: refetchFinished } = finishedQuery
  const previousRunning = useRef<{ page: number; ids: readonly string[] }>({
    page: 0,
    ids: [],
  })
  const runningResponsePage = feed.runningNav.page
  useEffect(() => {
    const currentIds = feed.running.map((operation) => operation.id)
    const previous = previousRunning.current
    if (
      previous.page === runningResponsePage &&
      operationsLeftRunning(previous.ids, currentIds)
    ) {
      void refetchFinished()
    }
    previousRunning.current = { page: runningResponsePage, ids: currentIds }
  }, [feed.running, runningResponsePage, refetchFinished])

  // Keep relative times fresh while the tray is visible.
  useEffect(() => {
    if (!open) return
    const timer = window.setInterval(() => setNow(Date.now()), 15_000)
    return () => window.clearInterval(timer)
  }, [open])

  const runningCount = feed.runningCount
  const badgeCount = runningCount + localCount
  const badge = operationsBadgeText(badgeCount)

  const handleOpenChange = (next: boolean) => {
    if (next) setNow(Date.now())
    setOperationsTrayOpen(next)
  }

  return (
    <Popover open={open} onOpenChange={handleOpenChange}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          size="icon"
          className="relative"
          aria-label={operationsTriggerLabel(badgeCount)}
          title="Operations"
        >
          <Activity className="size-4" />
          {badge !== null && (
            <span
              className="absolute -right-1 -top-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-primary px-1 text-[10px] font-semibold leading-none text-primary-foreground tabular-nums ring-2 ring-background"
              aria-hidden="true"
            >
              {badge}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" sideOffset={6} className="w-[360px] p-0">
        <OperationsTrayPanel
          running={{
            operations: feed.running,
            nav: feed.runningNav,
            isPending: runningQuery.isPending,
            isError: runningQuery.isError,
            errorMessage: runningQuery.isError
              ? problemDetail(
                  runningQuery.error,
                  'The running operations feed is unavailable.'
                )
              : null,
            onRetry: () => void runningQuery.refetch(),
            isPaging: runningQuery.isPlaceholderData,
            onNewer: () => setOperationsTrayPage('running', runningPage - 1),
            onOlder: () => setOperationsTrayPage('running', runningPage + 1),
          }}
          recent={{
            operations: feed.recent,
            nav: feed.recentNav,
            isPending: finishedQuery.isPending,
            isError: finishedQuery.isError,
            errorMessage: finishedQuery.isError
              ? problemDetail(
                  finishedQuery.error,
                  'The operations history is unavailable.'
                )
              : null,
            onRetry: () => void finishedQuery.refetch(),
            isPaging: finishedQuery.isPlaceholderData,
            onNewer: () => setOperationsTrayPage('recent', recentPage - 1),
            onOlder: () => setOperationsTrayPage('recent', recentPage + 1),
          }}
          localOperations={localOperations}
          runningCount={runningCount}
          onNavigate={() => setOperationsTrayOpen(false)}
          now={now}
        />
      </PopoverContent>
    </Popover>
  )
}
