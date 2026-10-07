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
 * matter how much newer finished work exists.
 *
 * Polling is on only when it is useful: every 5s while the popover is open,
 * every 10s while something is running, otherwise not at all (one fetch on
 * mount plus window-focus refetches).
 */
import { problemDetail } from '@/lib/api-problem'
import {
  operationsBadgeText,
  operationsLeftRunning,
  operationsPollInterval,
  operationsTrayFeed,
  operationsTriggerLabel,
} from '@/lib/operations'
import { useInfiniteQuery } from '@tanstack/react-query'
import { Activity } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { Button } from '../ui/button'
import { Popover, PopoverContent, PopoverTrigger } from '../ui/popover'
import {
  finishedOperationsInfiniteOptions,
  runningOperationsInfiniteOptions,
} from './operations-queries'
import { OperationsTrayPanel } from './OperationsTrayPanel'
import {
  setOperationsTrayOpen,
  useLocalOperations,
  useOperationsTrayOpen,
} from './operations-tray-store'

export function OperationsTray() {
  const open = useOperationsTrayOpen()
  const localOperations = useLocalOperations()
  const [now, setNow] = useState(() => Date.now())

  const localCount = localOperations.length

  const runningQuery = useInfiniteQuery({
    ...runningOperationsInfiniteOptions(),
    refetchInterval: (q) =>
      operationsPollInterval({
        open,
        runningCount: q.state.data?.pages[0]?.running_count ?? 0,
        localCount,
      }),
    refetchOnWindowFocus: true,
  })
  const serverRunningCount = runningQuery.data?.pages[0]?.running_count ?? 0

  // History follows the same policy, keyed on the running feed's count, so a
  // row that finishes while the tray is closed is already in "Recent" when
  // it opens.
  const finishedQuery = useInfiniteQuery({
    ...finishedOperationsInfiniteOptions(),
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
        runningPages: runningQuery.data?.pages,
        finishedPages: finishedQuery.data?.pages,
      }),
    [runningQuery.data, finishedQuery.data]
  )

  // When something leaves the running feed, pull history right away so the
  // finished row is there without waiting for a poll (which may now be off).
  const { refetch: refetchFinished } = finishedQuery
  const previousRunningIds = useRef<readonly string[]>([])
  useEffect(() => {
    const currentIds = feed.running.map((operation) => operation.id)
    if (operationsLeftRunning(previousRunningIds.current, currentIds)) {
      void refetchFinished()
    }
    previousRunningIds.current = currentIds
  }, [feed.running, refetchFinished])

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
            notLoaded: feed.runningNotLoaded,
            isPending: runningQuery.isPending,
            isError: runningQuery.isError,
            errorMessage: runningQuery.isError
              ? problemDetail(
                  runningQuery.error,
                  'The running operations feed is unavailable.'
                )
              : null,
            onRetry: () => void runningQuery.refetch(),
            hasMore: runningQuery.hasNextPage,
            isFetchingMore: runningQuery.isFetchingNextPage,
            onLoadMore: () => void runningQuery.fetchNextPage(),
          }}
          recent={{
            operations: feed.recent,
            isPending: finishedQuery.isPending,
            isError: finishedQuery.isError,
            errorMessage: finishedQuery.isError
              ? problemDetail(
                  finishedQuery.error,
                  'The operations history is unavailable.'
                )
              : null,
            onRetry: () => void finishedQuery.refetch(),
            hasMore: finishedQuery.hasNextPage,
            isFetchingMore: finishedQuery.isFetchingNextPage,
            onLoadMore: () => void finishedQuery.fetchNextPage(),
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
