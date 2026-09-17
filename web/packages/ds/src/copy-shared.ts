// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useRef, useState } from 'react'
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
  {
    onCopied,
    hold = COPY_HOLD_MS,
  }: { onCopied?: (value: string) => void; hold?: number } = {}
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
      setReason(
        e instanceof Error && e.message
          ? e.message
          : 'The browser blocked the copy. Select the text and copy it by hand.'
      )
    }
    timer.current = window.setTimeout(() => setState('idle'), hold)
  }, [value, onCopied, hold])

  return { state, reason, copy }
}
