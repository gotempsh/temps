// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, type RefObject } from 'react'
import { isFocusComposerShortcut } from './assistant-view'

/**
 * Pressing "/" anywhere outside a text field focuses the message box, the same
 * shortcut search boxes use. It is an accelerator only: the box is always
 * reachable by pointer and tab.
 */
export function useFocusComposerShortcut(
  ref: RefObject<HTMLTextAreaElement | null>,
  enabled = true
): void {
  useEffect(() => {
    if (!enabled) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (!isFocusComposerShortcut(event)) return
      const textarea = ref.current
      if (!textarea || textarea.disabled) return
      event.preventDefault()
      textarea.focus()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [ref, enabled])
}
