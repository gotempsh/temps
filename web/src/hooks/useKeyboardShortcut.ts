import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'

interface KeyboardShortcutOptions {
  /**
   * The key to listen for (e.g., 'n', 'c', 'e')
   */
  key: string
  /**
   * The path to navigate to when the key is pressed
   */
  path?: string
  /**
   * Custom callback to execute instead of navigation
   */
  callback?: () => void
  /**
   * Whether the shortcut is enabled (default: true)
   */
  enabled?: boolean
}

/**
 * Hook to register keyboard shortcuts that trigger navigation or callbacks.
 * Prevents triggering when user is typing in input fields.
 *
 * @example
 * // Navigate to create page on 'N' key
 * useKeyboardShortcut({ key: 'n', path: '/projects/new' })
 *
 * @example
 * // Execute custom callback on 'N' key
 * useKeyboardShortcut({ key: 'n', callback: () => setDialogOpen(true) })
 */
export function useKeyboardShortcut({
  key,
  path,
  callback,
  enabled = true,
}: KeyboardShortcutOptions) {
  const navigate = useNavigate()

  useEffect(() => {
    if (!enabled) return

    const handleKeyDown = (e: KeyboardEvent) => {
      // Check if user is typing in an input field
      const target = e.target as HTMLElement
      const isTyping =
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable

      // Only trigger if not typing and no modifier keys are pressed
      if (
        !isTyping &&
        e.key.toLowerCase() === key.toLowerCase() &&
        !e.metaKey &&
        !e.ctrlKey &&
        !e.altKey &&
        !e.shiftKey
      ) {
        e.preventDefault()

        if (callback) {
          callback()
        } else if (path) {
          navigate(path)
        }
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [key, path, callback, enabled, navigate])
}
