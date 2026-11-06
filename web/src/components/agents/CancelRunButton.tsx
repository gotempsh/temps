// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { Loader2 } from 'lucide-react'
import { useCancelRun } from './useCancelRun'

/**
 * Cancels an active Autopilot run. While the request is pending the button
 * looks inert and ignores clicks, but it stays focusable: a native `disabled`
 * would drop keyboard focus to the page the moment cancelling started.
 */
export function CancelRunButton({
  projectId,
  runId,
  onCancelled,
}: {
  projectId: number
  runId: number
  onCancelled: () => void
}) {
  const cancel = useCancelRun(projectId, runId, onCancelled)
  return (
    <Button
      type="button"
      size="sm"
      variant="destructive"
      aria-disabled={cancel.isPending}
      className={cn(cancel.isPending && 'cursor-not-allowed opacity-50')}
      onClick={() => {
        if (!cancel.isPending) cancel.mutate()
      }}
    >
      {cancel.isPending && (
        <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />
      )}
      Cancel
    </Button>
  )
}
