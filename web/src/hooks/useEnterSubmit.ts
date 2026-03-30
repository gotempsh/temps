import { useCallback } from 'react'

/**
 * Returns an onKeyDown handler that triggers the given callback on Enter,
 * unless the target is a textarea or button.
 */
export function useEnterSubmit(onSubmit: () => void) {
  return useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key !== 'Enter') return

      const target = e.target as HTMLElement
      const tag = target.tagName.toLowerCase()

      // Don't intercept Enter in textareas (they need newlines)
      // or buttons (they have their own Enter behavior)
      if (tag === 'textarea' || tag === 'button') return

      // Don't intercept if inside a select/combobox popover
      if (target.closest('[role="listbox"]') || target.closest('[role="option"]')) return

      e.preventDefault()
      onSubmit()
    },
    [onSubmit]
  )
}
