// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, useState, type ReactNode } from 'react'
import { X } from 'lucide-react'
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  Input,
} from '@temps-sdk/ui'
import { Button } from './button'
import { CopyAction } from './copy-action'
import { cn } from './lib/cn'

export interface EchoDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: ReactNode
  description: ReactNode
  /** The exact phrase the operator must retype — usually the resource's name or slug. */
  phrase: string
  /** Label for the destructive action, e.g. "Delete project". */
  confirmLabel: string
  onConfirm: () => void | Promise<void>
  busy?: boolean
}

/**
 * Typed-confirmation gate for irreversible actions (delete a project, revoke
 * every session, drop a database). The confirm button stays wired but inert
 * until the input matches `phrase` exactly — see RULES.md § "wired-control
 * rule": a control that can't act yet must look inert, not merely be
 * disabled, so this shows a plain button with a muted state rather than the
 * native `disabled` attribute, matching `Button`'s own `busy` convention.
 */
export function EchoDialog({
  open,
  onOpenChange,
  title,
  description,
  phrase,
  confirmLabel,
  onConfirm,
  busy = false,
}: EchoDialogProps) {
  const [value, setValue] = useState('')
  const inputId = useId()
  const matches = value === phrase

  const close = (next: boolean) => {
    if (!next) setValue('')
    onOpenChange(next)
  }

  return (
    <AlertDialog open={open} onOpenChange={close}>
      <AlertDialogContent>
        <AlertDialogCancel asChild>
          <button
            type="button"
            aria-label="Close"
            className="absolute right-4 top-4 rounded-sm opacity-70 ring-offset-background transition-opacity hover:opacity-100 focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:pointer-events-none"
          >
            <X className="size-4" aria-hidden />
          </button>
        </AlertDialogCancel>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        <div className="space-y-2">
          <div className="text-sm text-muted-foreground">
            Type{' '}
            <span className="inline-flex items-center gap-1 align-middle">
              <span className="rounded border border-dashed bg-muted/40 px-1 font-mono font-semibold text-foreground">
                {phrase}
              </span>
              <CopyAction value={phrase} label="Copy confirmation name" />
            </span>{' '}
            to confirm.
          </div>
          <Input
            id={inputId}
            aria-label={`Type ${phrase} to confirm`}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </div>
        <AlertDialogFooter>
          <Button variant="outline" onClick={() => close(false)}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            className={cn(!matches && 'pointer-events-none opacity-50')}
            aria-disabled={!matches}
            busy={busy}
            busyLabel={confirmLabel}
            onClick={() => {
              if (!matches) return
              void onConfirm()
            }}
          >
            {confirmLabel}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
