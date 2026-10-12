// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
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
import { buttonVariants } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import {
  confirmDeleteCopy,
  type ConfirmDeleteCopyInput,
} from '@/components/ui/confirm-delete'

export interface ConfirmDeleteDialogProps extends ConfirmDeleteCopyInput {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Called only when the user presses the confirm button. */
  onConfirm: () => void
  /** Replaces the default consequence sentence (still names nothing else). */
  description?: ReactNode
}

/**
 * Controlled confirmation for a destructive delete. Always mounted by the
 * caller and driven through `open`, so it never conditionally mounts.
 *
 * While `isPending` the dialog cannot be dismissed and the confirm button
 * shows its pending label; the caller closes it from the mutation's
 * `onSuccess`, so a failed delete leaves it open with the error toast.
 */
export function ConfirmDeleteDialog({
  open,
  onOpenChange,
  onConfirm,
  description,
  ...copyInput
}: ConfirmDeleteDialogProps) {
  const copy = confirmDeleteCopy(copyInput)
  const isPending = copyInput.isPending ?? false

  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        if (isPending && !next) return
        onOpenChange(next)
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{copy.title}</AlertDialogTitle>
          <AlertDialogDescription>
            {description ?? copy.description}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={isPending}>Cancel</AlertDialogCancel>
          <AlertDialogAction
            disabled={isPending}
            className={cn(buttonVariants({ variant: 'destructive' }))}
            onClick={(event) => {
              // Radix closes the dialog on Action click; keep it open until
              // the request settles so pending/error state stays visible.
              event.preventDefault()
              onConfirm()
            }}
          >
            {copy.confirmLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
