// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ReactNode } from 'react'
import { Button } from './button'
import { cn } from './lib/cn'

/**
 * The row that ends every group of settings: a rule, a hint on the left and
 * Save on the right. Card footers on a settings surface use the same classes,
 * so a group saved by a card and a group saved by its page look alike.
 * `[&>:last-child]:ml-auto` keeps a lone Save on the right.
 */
export const saveRowClassName =
  'flex flex-wrap items-center gap-x-4 gap-y-2 border-t pt-4 text-sm text-muted-foreground [&>:last-child]:ml-auto'

/**
 * Save for a group of settings, disabled until something changed. `sticky`
 * keeps a page-wide save in view while a long form scrolls.
 */
export function SaveRow({
  dirty,
  saving = false,
  hint,
  label = 'Save',
  sticky = false,
  disabled = false,
  onSave,
  className,
}: {
  /** Whether anything in the group differs from what is saved. */
  dirty: boolean
  saving?: boolean
  /** What saving does or needs; defaults to whether there is anything to save. */
  hint?: ReactNode
  label?: ReactNode
  sticky?: boolean
  /** Blocks saving for another reason (invalid input, missing permission). */
  disabled?: boolean
  /** Saves on click; without it the button submits the enclosing form. */
  onSave?: () => void
  className?: string
}) {
  return (
    <div
      className={cn(
        saveRowClassName,
        sticky && 'sticky bottom-0 z-10 bg-background pb-3',
        className
      )}
    >
      <p>{hint ?? (dirty ? 'You have unsaved changes' : 'All changes saved')}</p>
      <Button
        type={onSave ? 'button' : 'submit'}
        size="sm"
        onClick={onSave}
        busy={saving}
        busyLabel="Saving…"
        disabled={!saving && (!dirty || disabled)}
      >
        {label}
      </Button>
    </div>
  )
}
