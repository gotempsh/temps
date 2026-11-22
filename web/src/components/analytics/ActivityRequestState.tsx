// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { AlertCircle, Loader2, RotateCcw, Timer } from 'lucide-react'
import type { ActivityOperation } from '@/api/client/types.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  OPERATION_LABEL,
  activityErrorMessage,
  activityProblem,
  formatElapsed,
  isActionableFailure,
} from './activity-request-state'
import type { ActivityGateState } from './use-activity-gate'

/**
 * What happened to the last attempt at an AI action and what the operator can
 * do next: progress while it runs, a countdown while it cools down, or the
 * provider's failure with a link to fix it and a retry.
 */
export function ActivityRequestState({
  operation,
  gate,
  isPending,
  error,
  onRetry,
}: {
  operation: ActivityOperation
  gate: ActivityGateState
  isPending: boolean
  error: unknown
  onRetry?: () => void
}) {
  if (isPending) return null
  if (gate.running) {
    return (
      <p
        role="status"
        className="flex items-center gap-2 text-sm text-muted-foreground"
      >
        <Loader2 className="size-4 shrink-0 animate-spin" aria-hidden="true" />
        {OPERATION_LABEL[gate.running.operation]} is running for this project ·
        started {formatElapsed(gate.running.startedAt, gate.now)} ago
      </p>
    )
  }
  if (gate.cooldownSecs > 0) {
    return (
      <p
        role="status"
        className="flex items-center gap-2 text-sm text-muted-foreground"
      >
        <Timer className="size-4 shrink-0" aria-hidden="true" />
        {OPERATION_LABEL[operation]} is available again in {gate.cooldownSecs}s
      </p>
    )
  }
  if (!error || !isActionableFailure(error)) return null
  const setupUrl = activityProblem(error)?.setup_url
  return (
    <Alert variant="destructive">
      <AlertCircle className="h-4 w-4" />
      <AlertDescription className="space-y-3">
        <p className="[overflow-wrap:anywhere]">
          {activityErrorMessage(error)}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          {onRetry && (
            <Button type="button" variant="outline" size="sm" onClick={onRetry}>
              <RotateCcw className="mr-2 size-4" aria-hidden="true" />
              Try again
            </Button>
          )}
          {setupUrl && (
            <Button type="button" variant="link" size="sm" asChild>
              <Link to={setupUrl}>Open AI Gateway settings</Link>
            </Button>
          )}
        </div>
      </AlertDescription>
    </Alert>
  )
}
