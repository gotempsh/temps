// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Header operations tray: one place to follow deployments, rollbacks,
 * promotions, restores, backups and autofix runs, backed by `GET /operations`
 * so it survives a refresh.
 *
 * Polling is on only when it is useful: every 5s while the popover is open,
 * every 10s while something is running, otherwise not at all (one fetch on
 * mount plus window-focus refetches).
 */
import { listOperationsOptions } from '@/api/client/@tanstack/react-query.gen'
import { problemDetail } from '@/lib/api-problem'
import {
  OPERATIONS_TRAY_PAGE_SIZE,
  operationsBadgeText,
  operationsPollInterval,
  operationsTriggerLabel,
} from '@/lib/operations'
import { useQuery } from '@tanstack/react-query'
import { Activity } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Button } from '../ui/button'
import { Popover, PopoverContent, PopoverTrigger } from '../ui/popover'
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

  const query = useQuery({
    ...listOperationsOptions({
      query: { page: 1, page_size: OPERATIONS_TRAY_PAGE_SIZE },
    }),
    refetchInterval: (q) =>
      operationsPollInterval({
        open,
        runningCount: q.state.data?.running_count ?? 0,
        localCount: localOperations.length,
      }),
    refetchOnWindowFocus: true,
  })

  // Keep relative times fresh while the tray is visible.
  useEffect(() => {
    if (!open) return
    const timer = window.setInterval(() => setNow(Date.now()), 15_000)
    return () => window.clearInterval(timer)
  }, [open])

  const runningCount = query.data?.running_count ?? 0
  const badgeCount = runningCount + localOperations.length
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
          operations={query.data?.operations ?? []}
          localOperations={localOperations}
          runningCount={runningCount}
          isPending={query.isPending}
          isError={query.isError}
          errorMessage={
            query.isError
              ? problemDetail(
                  query.error,
                  'The operations feed is unavailable.'
                )
              : null
          }
          onRetry={() => void query.refetch()}
          onNavigate={() => setOperationsTrayOpen(false)}
          now={now}
        />
      </PopoverContent>
    </Popover>
  )
}
