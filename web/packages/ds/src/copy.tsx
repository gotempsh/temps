// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useRef, useState, type ButtonHTMLAttributes, type ReactNode } from 'react'
import { Check, Copy, X } from 'lucide-react'
import { cn } from './lib/cn'
import { writeToClipboard } from './lib/clipboard'

/**
 * A copy answers on the control that was pressed, not in a toast.
 *
 * A toast lands in a corner the reader is not looking at, and it says
 * "copied" whether or not anything reached the clipboard. The control
 * itself is where the eye already is, so it is the one place the answer
 * cannot be missed: `copied` for two seconds, or `couldn't copy` in red
 * with the reason on hover (a self-hosted instance reached over plain
 * http on a LAN address has no `navigator.clipboard`). A checkmark shown
 * over an empty clipboard is only discovered later, by pasting the wrong
 * thing somewhere it matters, so a failure is never dressed as a success.
 */
export type CopyState = 'idle' | 'copied' | 'failed'

/** How long the answer stays on the control before it reads "copy" again. */
export const COPY_HOLD_MS = 2000

export function useCopy(
  value: string | (() => string),
  { onCopied, hold = COPY_HOLD_MS }: { onCopied?: (value: string) => void; hold?: number } = {},
) {
  const [state, setState] = useState<CopyState>('idle')
  const [reason, setReason] = useState<string | undefined>()
  const timer = useRef<number | undefined>(undefined)

  useEffect(() => () => window.clearTimeout(timer.current), [])

  const copy = useCallback(async () => {
    const text = typeof value === 'function' ? value() : value
    window.clearTimeout(timer.current)
    try {
      await writeToClipboard(text)
      setState('copied')
      setReason(undefined)
      onCopied?.(text)
    } catch (e) {
      setState('failed')
      setReason(e instanceof Error && e.message ? e.message : 'The browser blocked the copy. Select the text and copy it by hand.')
    }
    timer.current = window.setTimeout(() => setState('idle'), hold)
  }, [value, onCopied, hold])

  return { state, reason, copy }
}

/**
 * The button form of `useCopy`. The idle label sets the width and stays in
 * the layout invisibly while the answer is shown, so a row never moves when
 * "copy link" becomes "copied". Pass the sizing classes the row uses; the
 * border, the hover and the states are the control's own.
 */
export function CopyAction({
  value, children, copiedLabel = 'copied', failedLabel = "couldn't copy", onCopied, className, ...rest
}: {
  /** What goes to the clipboard, or a function that builds it at press time. */
  value: string | (() => string)
  /** The idle label: an icon and a verb, "copy link", "copy line". */
  children: ReactNode
  copiedLabel?: ReactNode
  failedLabel?: ReactNode
  onCopied?: (value: string) => void
  className?: string
} & Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'onClick' | 'value' | 'children'>) {
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
        className,
      )}
      {...rest}
    >
      <span className="grid">
        <span aria-hidden={state !== 'idle'} className={cn('inline-flex items-center gap-1 [grid-area:1/1]', state !== 'idle' && 'invisible')}>{children}</span>
        <span role="status" aria-live="polite" className={cn('inline-flex items-center gap-1 [grid-area:1/1]', state === 'idle' && 'invisible')}>
          {state === 'copied' && <><Check aria-hidden className="h-3 w-3" />{copiedLabel}</>}
          {state === 'failed' && <><X aria-hidden className="h-3 w-3" />{failedLabel}</>}
        </span>
      </span>
    </button>
  )
}

/** The default idle icon, for callers that want the same glyph the gallery shows. */
export const CopyIcon = Copy
