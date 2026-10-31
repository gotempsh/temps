// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from 'react'
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
import { ConfirmNameBadge } from '@/components/ui/confirm-name-badge'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { cn } from '@/lib/utils'

export interface ConfirmDeleteDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** What kind of thing is going away, lower case: "email provider". */
  resourceType: string
  /**
   * The specific thing going away, as the user knows it ("Postmark",
   * `API_KEY`, `example.com`). Required: a confirmation that cannot name its
   * target is the bug this component exists to prevent. Pass the name from the
   * row/record being acted on, never a constant.
   */
  resourceName: string
  /** What else is affected (dependents, data that goes with it). */
  consequences?: ReactNode
  /** Verb in the title and on the button. Defaults to "Delete". */
  verb?: string
  /** Overrides the confirm button label (default: the verb). */
  confirmLabel?: string
  /** Set to false for reversible actions. Default: "This cannot be undone." */
  irreversible?: boolean
  /**
   * Require the user to type `resourceName` before the button enables. Use for
   * resources whose loss is expensive to recover (projects, databases).
   */
  requireTypedName?: boolean
  /**
   * When provided, the dialog stays open while `true` (the button shows a
   * pending label and Cancel is disabled) and the caller closes it when the
   * mutation settles. When omitted, the dialog closes as soon as the user
   * confirms.
   */
  isPending?: boolean
  /**
   * Where focus goes when the dialog closes. Pass it when the dialog is
   * opened from a menu item: the item is gone by then, so the control that
   * opened the menu (the row's "More actions" button) is the place to return
   * to. Defaults to whatever had focus when the dialog opened.
   */
  returnFocusRef?: RefObject<HTMLElement | null>
  onConfirm: () => void
}

/** Text shown for a record whose name is missing, so the title never reads "Delete ?". */
const UNNAMED = 'this item'

/**
 * Confirmation for removing something, always naming what is removed.
 *
 * Controlled (`open`) rather than conditionally mounted. Keep the record being
 * acted on in the caller's state, pass `open={target !== null}`, and send the
 * request only from `onConfirm`. Every delete/remove/revoke control in the
 * console goes through this component; `confirm-delete-guard.test.ts` fails the
 * build when a file that fires such a mutation does not.
 */
export function ConfirmDeleteDialog(props: ConfirmDeleteDialogProps) {
  const { open, onOpenChange, isPending, returnFocusRef } = props
  // Callers open this from a plain button rather than an AlertDialogTrigger,
  // so Radix has no trigger to hand focus back to on close. Remember what had
  // focus before the dialog took it (layout effects run before Radix's focus
  // scope moves focus) and restore it, so keyboard users keep their place.
  const opener = useRef<HTMLElement | null>(null)
  useLayoutEffect(() => {
    if (open && document.activeElement instanceof HTMLElement) {
      opener.current = document.activeElement
    }
  }, [open])
  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        if (isPending === true && !next) return
        onOpenChange(next)
      }}
    >
      <AlertDialogContent
        // The dialog is a one-column grid whose track defaults to min-content,
        // so a long unbroken name (a webhook URL) would widen it past the
        // viewport and push the buttons off-screen. Pin the track to the
        // dialog width and let names wrap instead.
        className="grid-cols-[minmax(0,1fr)]"
        onCloseAutoFocus={(event) => {
          event.preventDefault()
          const target = returnFocusRef?.current?.isConnected
            ? returnFocusRef.current
            : opener.current
          if (target?.isConnected) target.focus()
        }}
      >
        <ConfirmDeleteBody {...props} />
      </AlertDialogContent>
    </AlertDialog>
  )
}

/**
 * Lives inside the dialog content, which Radix unmounts when closed, so the
 * typed-name field starts empty every time the dialog opens.
 */
function ConfirmDeleteBody({
  resourceType,
  resourceName,
  consequences,
  verb = 'Delete',
  confirmLabel,
  irreversible = true,
  requireTypedName = false,
  isPending,
  onConfirm,
}: ConfirmDeleteDialogProps) {
  const [typed, setTyped] = useState('')
  const name = resourceName.trim()
  const named = name.length > 0
  const typedMatches = !requireTypedName || (named && typed === name)
  const holdOpen = isPending !== undefined
  const busy = isPending === true

  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle className="[overflow-wrap:anywhere]">
          {`${verb} ${resourceType} ${named ? name : UNNAMED}?`}
        </AlertDialogTitle>
        <AlertDialogDescription asChild>
          <div className="space-y-2">
            <p>
              {verb} {resourceType}{' '}
              <span className="font-medium text-foreground break-all">
                {named ? name : UNNAMED}
              </span>
              .
            </p>
            {consequences ? (
              // Focusable so keyboard users can scroll long lists: Radix's
              // focus loop only visits tabbable elements.
              <div className="max-h-48 overflow-y-auto" tabIndex={0}>
                {consequences}
              </div>
            ) : null}
            {irreversible ? <p>This cannot be undone.</p> : null}
          </div>
        </AlertDialogDescription>
      </AlertDialogHeader>
      {requireTypedName && named ? (
        <div className="space-y-2">
          <Label htmlFor="confirm-delete-name" className="text-sm">
            Type <ConfirmNameBadge value={name} /> to confirm
          </Label>
          <Input
            id="confirm-delete-name"
            autoComplete="off"
            value={typed}
            disabled={busy}
            onChange={(e) => setTyped(e.target.value)}
          />
        </div>
      ) : null}
      <AlertDialogFooter>
        <AlertDialogCancel disabled={busy}>Cancel</AlertDialogCancel>
        <AlertDialogAction
          className={cn(buttonVariants({ variant: 'destructive' }))}
          disabled={busy || !typedMatches}
          onClick={(e) => {
            if (holdOpen) e.preventDefault()
            if (!typedMatches) return
            onConfirm()
          }}
        >
          {busy ? 'Working…' : (confirmLabel ?? verb)}
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  )
}
