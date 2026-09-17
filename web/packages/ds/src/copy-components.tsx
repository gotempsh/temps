// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ButtonHTMLAttributes, type ReactNode } from 'react'
import { Check, Copy, X } from 'lucide-react'
import { cn } from './lib/cn'
import { useCopy } from './copy-shared'

/**
 * The button form of `useCopy`. The idle label sets the width and stays in
 * the layout invisibly while the answer is shown, so a row never moves when
 * "copy link" becomes "copied". Pass the sizing classes the row uses; the
 * border, the hover and the states are the control's own.
 */
export function CopyAction({
  value,
  children,
  copiedLabel = 'copied',
  failedLabel = "couldn't copy",
  onCopied,
  className,
  ...rest
}: {
  /** What goes to the clipboard, or a function that builds it at press time. */
  value: string | (() => string)
  /** The idle label: an icon and a verb, "copy link", "copy line". */
  children: ReactNode
  copiedLabel?: ReactNode
  failedLabel?: ReactNode
  onCopied?: (value: string) => void
  className?: string
} & Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  'onClick' | 'value' | 'children'
>) {
  const { state, reason, copy } = useCopy(value, { onCopied })
  return (
    <button
      type="button"
      onClick={copy}
      data-state={state}
      title={state === 'failed' ? reason : rest.title}
      className={cn(
        'inline-flex h-6 items-center border px-2 text-[11px] hover:bg-muted',
        state === 'failed' && 'border-destructive text-destructive',
        className
      )}
      {...rest}
    >
      <span className="grid">
        <span
          aria-hidden={state !== 'idle'}
          className={cn(
            'inline-flex items-center gap-1 [grid-area:1/1]',
            state !== 'idle' && 'invisible'
          )}
        >
          {children}
        </span>
        <span
          role="status"
          aria-live="polite"
          className={cn(
            'inline-flex items-center gap-1 [grid-area:1/1]',
            state === 'idle' && 'invisible'
          )}
        >
          {state === 'copied' && (
            <>
              <Check aria-hidden className="h-3 w-3" />
              {copiedLabel}
            </>
          )}
          {state === 'failed' && (
            <>
              <X aria-hidden className="h-3 w-3" />
              {failedLabel}
            </>
          )}
        </span>
      </span>
    </button>
  )
}

/** The default idle icon, for callers that want the same glyph the gallery shows. */
export const CopyIcon = Copy
