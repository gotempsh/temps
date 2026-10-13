// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import {
  RECOVERY_AFFECTED_QUERY_IDS,
  recoveryActionCopy,
  runRecoveryAction,
  type RecoveryAction,
} from '@/lib/recovery-actions'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { toast } from 'sonner'

interface RecoveryActionDialogProps {
  /** The action to confirm; `null` keeps the dialog closed. */
  action: RecoveryAction | null
  onClose: () => void
  /** Called after the API accepted the action, e.g. to navigate. */
  onDone?: (action: RecoveryAction) => void
}

function isAffectedQuery(queryKey: readonly unknown[]): boolean {
  const head = queryKey[0] as { _id?: unknown } | undefined
  return (
    typeof head?._id === 'string' && RECOVERY_AFFECTED_QUERY_IDS.has(head._id)
  )
}

/**
 * Confirmation for a one-click remedy (redeploy, roll back, restart a
 * container). Always mounted and driven by `action`, so callers keep one
 * piece of state per surface instead of a dialog per button.
 */
export function RecoveryActionDialog({
  action,
  onClose,
  onDone,
}: RecoveryActionDialogProps) {
  const queryClient = useQueryClient()
  const copy = action ? recoveryActionCopy(action) : null

  const mutation = useMutation({
    mutationFn: (pending: RecoveryAction) => runRecoveryAction(pending),
    meta: { errorTitle: copy?.errorTitle },
    onSuccess: (_, pending) => {
      toast.success(recoveryActionCopy(pending).success)
      void queryClient.invalidateQueries({
        predicate: (query) => isAffectedQuery(query.queryKey),
      })
      onClose()
      onDone?.(pending)
    },
  })

  return (
    <AlertDialog
      open={action != null}
      onOpenChange={(open) => {
        if (!open && !mutation.isPending) onClose()
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{copy?.title}</AlertDialogTitle>
          <AlertDialogDescription>{copy?.description}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={mutation.isPending}>
            Cancel
          </AlertDialogCancel>
          <AlertDialogAction
            disabled={mutation.isPending}
            onClick={(event) => {
              // Keep the dialog open until the request settles.
              event.preventDefault()
              if (action) mutation.mutate(action)
            }}
          >
            {mutation.isPending && (
              <Loader2 className="mr-2 size-4 animate-spin" />
            )}
            {copy?.confirmLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
