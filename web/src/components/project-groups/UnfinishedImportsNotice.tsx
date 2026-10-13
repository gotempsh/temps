// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { AlertTriangle, Loader2, RefreshCw } from 'lucide-react'
import { listInstantiationsOptions } from '@/api/client/@tanstack/react-query.gen'
import { retryInstantiation } from '@/api/client/sdk.gen'
import type { InstantiationResponse, ProjectGroupResponse } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { getErrorMessage } from '@/utils/errorHandling'
import { useInvalidateGroups } from './useProjectGroups'

const POLL_INTERVAL_MS = 2000
const DISMISSED_KEY = 'temps.dismissedInstantiations'

function loadDismissed(): Set<number> {
  try {
    const raw = localStorage.getItem(DISMISSED_KEY)
    return new Set(raw ? (JSON.parse(raw) as number[]) : [])
  } catch {
    return new Set()
  }
}

/**
 * Imports and template creations into this project that stopped before
 * finishing. The server keeps them, and every step already taken, so they
 * can be continued after the dialog that started them is gone.
 */
export function UnfinishedImportsNotice({
  group,
}: {
  group: ProjectGroupResponse
}) {
  const invalidate = useInvalidateGroups()
  const query = useQuery({
    // The newest page is enough: older unfinished ones were dismissed or
    // retried long ago, and each stays reachable through the CLI.
    ...listInstantiationsOptions({
      query: { group_id: group.id, page: 1, per_page: 20 },
    }),
    // Follow a retry until it finishes.
    refetchInterval: (q) =>
      (q.state.data?.items ?? []).some(
        (item) => item.status === 'running' || item.status === 'pending'
      )
        ? POLL_INTERVAL_MS
        : false,
  })
  const [retrying, setRetrying] = useState<number | null>(null)
  const [dismissed, setDismissed] = useState(loadDismissed)
  const dismiss = (id: number) => {
    const next = new Set(dismissed).add(id)
    setDismissed(next)
    try {
      localStorage.setItem(DISMISSED_KEY, JSON.stringify([...next].slice(-100)))
    } catch {
      // Private mode: dismissed for this page only.
    }
  }

  const items = (query.data?.items ?? []).filter(
    (item) => !dismissed.has(item.id) && item.status !== 'succeeded'
  )

  if (query.isError && !query.data) {
    return (
      <p className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <AlertTriangle className="size-3.5" />
        Unfinished imports into this project could not be checked.
        <Button
          size="sm"
          variant="link"
          className="h-auto p-0 text-xs"
          disabled={query.isFetching}
          onClick={() => void query.refetch()}
        >
          Retry
        </Button>
      </p>
    )
  }
  if (items.length === 0) return null

  const retry = async (item: InstantiationResponse) => {
    setRetrying(item.id)
    try {
      await retryInstantiation({ path: { id: item.id }, throwOnError: true })
      await query.refetch()
    } catch (err) {
      toast.error(getErrorMessage(err, 'Could not retry it'))
    } finally {
      setRetrying(null)
      await invalidate()
    }
  }

  return (
    <div className="space-y-2">
      {items.map((item) => {
        const done = item.steps.filter((s) => s.status === 'done').length
        const failedStep = item.steps.find((s) => s.status === 'failed')
        const running = item.status === 'running' || item.status === 'pending'
        return (
          <Alert key={item.id} variant={running ? 'default' : 'destructive'}>
            {running ? (
              <Loader2 className="size-4 animate-spin" />
            ) : (
              <AlertTriangle className="size-4" />
            )}
            <AlertTitle>
              {item.status === 'pending'
                ? `${item.name}: waiting to start`
                : running
                  ? `${item.name}: ${done} of ${item.steps.length} steps done`
                  : `${item.name} did not finish (${done} of ${item.steps.length} steps done)`}
            </AlertTitle>
            <AlertDescription className="space-y-2">
              {!running && (
                <p>
                  {item.status === 'interrupted'
                    ? 'The server restarted while it ran.'
                    : failedStep
                      ? `${failedStep.label}: ${failedStep.error ?? 'failed'}`
                      : (item.error ?? 'A step failed.')}{' '}
                  Retrying continues from there; services already created are
                  kept.
                </p>
              )}
              {running && (
                <p>
                  It runs on the server; you can leave this page. Dismissing
                  only hides this notice.
                </p>
              )}
              <div className="flex gap-2">
                {!running && (
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={retrying !== null}
                    onClick={() => void retry(item)}
                  >
                    {retrying === item.id ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <RefreshCw className="size-4" />
                    )}
                    Retry
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={retrying === item.id}
                  onClick={() => dismiss(item.id)}
                >
                  Dismiss
                </Button>
              </div>
            </AlertDescription>
          </Alert>
        )
      })}
    </div>
  )
}
