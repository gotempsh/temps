// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback } from 'react'

/**
 * Whether Enter on this element should submit the form. Textareas need Enter
 * for newlines, and buttons and links have their own Enter action (a link in
 * help text must navigate, not submit); select/combobox popovers use Enter to
 * pick an option.
 */
export function shouldSubmitOnEnter(target: HTMLElement): boolean {
  const tag = target.tagName.toLowerCase()
  if (tag === 'textarea' || tag === 'button' || tag === 'a') return false
  if (target.closest('[role="listbox"]') || target.closest('[role="option"]'))
    return false
  return true
}

/**
 * Returns an onKeyDown handler that triggers the given callback on Enter,
 * unless the target handles Enter itself (see `shouldSubmitOnEnter`).
 */
export function useEnterSubmit(onSubmit: () => void) {
  return useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key !== 'Enter') return
      if (!shouldSubmitOnEnter(e.target as HTMLElement)) return

      e.preventDefault()
      onSubmit()
    },
    [onSubmit]
  )
}
