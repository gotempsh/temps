// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { Loader2 } from 'lucide-react'

/**
 * Cancels an active Autopilot run. While the cancel is `pending` the button
 * looks inert and ignores clicks, but it stays focusable: a native `disabled`
 * would drop keyboard focus to the page the moment cancelling started.
 *
 * Stateless on purpose: the page owns the cancel mutation (`useCancelRun`),
 * because this button unmounts once the run stops being active.
 */
export function CancelRunButton({
  pending,
  onCancel,
}: {
  pending: boolean
  onCancel: () => void
}) {
  return (
    <Button
      type="button"
      size="sm"
      variant="destructive"
      aria-disabled={pending}
      className={cn(pending && 'cursor-not-allowed opacity-50')}
      onClick={() => {
        if (!pending) onCancel()
      }}
    >
      {pending && <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />}
      Cancel
    </Button>
  )
}
