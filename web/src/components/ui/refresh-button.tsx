// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  isCancelledError,
  partialMatchKey,
  useIsFetching,
  useMutation,
  useQueryClient,
  type Query,
  type QueryKey,
} from '@tanstack/react-query'
import { notify } from '@temps-sdk/ds'
import { RefreshCw } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { problemDetail } from '@/lib/api-problem'
import { formatUpdatedAgo, moveWindows } from '@/lib/refresh'
import { isExpectedQueryFailure } from '@/lib/route-query-failures'
import { cn } from '@/lib/utils'

export interface RefreshButtonProps {
  /**
   * Keys of the queries this page shows. Each one matches as a prefix, the
   * way `queryClient.invalidateQueries({ queryKey })` does, so a generated
   * `getFooQueryKey({ path })` covers every page and filter of that list.
   * Matching queries on screen are refetched; cached ones that are not are
   * only marked stale. Nothing outside these keys is touched.
   */
  queryKeys: readonly QueryKey[]
  /**
   * Runs before the refetch, for pages whose window is relative to now
   * ("last 24 hours"): move the window here. It is applied synchronously,
   * so the refetch and the spinner cover the moved window's query.
   */
  onRefresh?: () => void
  className?: string
}

/**
 * The current time, re-read every `intervalMs` while `enabled`. Between ticks
 * it can lag behind a newer refresh; the caller clamps to the refresh time.
 */
function useNow(enabled: boolean, intervalMs: number) {
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    if (!enabled) return
    const timer = setInterval(() => setNow(Date.now()), intervalMs)
    return () => clearInterval(timer)
  }, [enabled, intervalMs])
  return now
}

/**
 * The page header's Refresh action: refetches the page's own queries, keeps
 * the rows on screen while it does, and says when the data was last loaded.
 *
 * Place it first in `PageHeader`'s `actions`. It never reloads the document or
 * the whole cache, and never changes the page's filters, sort or page.
 */
export function RefreshButton({
  queryKeys,
  onRefresh,
  className,
}: RefreshButtonProps) {
  const queryClient = useQueryClient()
  const matches = (query: Query) =>
    queryKeys.some((key) => partialMatchKey(query.queryKey, key))
  const fetching = useIsFetching({ predicate: matches }) > 0
  const [refreshedAt, setRefreshedAt] = useState<number | null>(null)
  const now = useNow(refreshedAt !== null, 30_000)

  const refresh = useMutation({
    mutationFn: async () => {
      const startedAt = Date.now()
      // `cancelRefetch: false` joins a fetch already in flight (a poll, or the
      // moved window's first load) instead of throwing it away.
      await queryClient.invalidateQueries(
        { predicate: matches, refetchType: 'active' },
        { cancelRefetch: false }
      )
      // A read the page works without (a role refused optional context) is
      // not a failed refresh; anything else that failed just now is.
      const [failed] = queryClient.getQueryCache().findAll({
        predicate: (query) =>
          matches(query) &&
          query.state.status === 'error' &&
          query.state.errorUpdatedAt >= startedAt &&
          !isExpectedQueryFailure(query),
      })
      if (failed) throw failed.state.error
      return Date.now()
    },
    onSuccess: (at) => setRefreshedAt(at),
    onError: (error) => {
      // Leaving the page mid-refresh cancels its queries; that is not a failure.
      if (isCancelledError(error)) return
      notify.fail(
        "Couldn't refresh",
        problemDetail(
          error,
          'The server did not answer. Check the connection and try again.'
        )
      )
    },
  })
  const busy = fetching || refresh.isPending

  const handleClick = () => {
    if (busy) return
    if (onRefresh) moveWindows(onRefresh)
    refresh.mutate()
  }

  return (
    <div className={cn('flex items-center gap-2', className)}>
      {refreshedAt !== null ? (
        <span className="text-xs text-muted-foreground tabular-nums">
          {formatUpdatedAgo(refreshedAt, Math.max(now, refreshedAt))}
        </span>
      ) : null}
      <TooltipProvider>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              type="button"
              variant="outline"
              size="sm"
              aria-busy={busy}
              aria-disabled={busy}
              className={cn(busy && 'cursor-wait')}
              onClick={handleClick}
            >
              <RefreshCw
                className={cn('size-4', busy && 'animate-spin')}
                aria-hidden="true"
              />
              <span className="max-sm:sr-only">Refresh</span>
            </Button>
          </TooltipTrigger>
          <TooltipContent>Refresh</TooltipContent>
        </Tooltip>
      </TooltipProvider>
    </div>
  )
}
