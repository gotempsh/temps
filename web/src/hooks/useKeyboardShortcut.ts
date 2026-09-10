// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef } from 'react'
import { useNavigate } from 'react-router'

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
 * Selector for the Radix overlays that own the keyboard while they're open.
 * A bare letter shortcut must not fire underneath one of these — otherwise
 * pressing `N` inside an edit dialog navigates the page out from under it.
 */
const OPEN_OVERLAY_SELECTOR = [
  '[role="dialog"][data-state="open"]',
  '[role="alertdialog"][data-state="open"]',
  '[role="menu"][data-state="open"]',
  '[role="listbox"][data-state="open"]',
].join(',')

/**
 * Hook to register keyboard shortcuts that trigger navigation or callbacks.
 * Prevents triggering when the user is typing in an input field or when a
 * dialog, menu or select is open on top of the page.
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

  // Callers pass an inline arrow (`onClick={() => setOpen(true)}`), so a new
  // identity every render. Keeping it in a ref stops the effect from tearing
  // the keydown listener down and re-adding it on each render. The write is
  // in an effect, not in render — mutating a ref during render is unsafe
  // under concurrent rendering.
  const callbackRef = useRef(callback)
  useEffect(() => {
    callbackRef.current = callback
  }, [callback])

  useEffect(() => {
    if (!enabled) return

    const handleKeyDown = (e: KeyboardEvent) => {
      // Check if user is typing in an input field
      const target = e.target as HTMLElement
      const isTyping =
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable

      // A dialog, menu or select on top of the page owns the keyboard.
      const overlayOpen = document.querySelector(OPEN_OVERLAY_SELECTOR) !== null

      // Only trigger if not typing and no modifier keys are pressed
      if (
        !isTyping &&
        !overlayOpen &&
        e.key.toLowerCase() === key.toLowerCase() &&
        !e.metaKey &&
        !e.ctrlKey &&
        !e.altKey &&
        !e.shiftKey
      ) {
        e.preventDefault()

        if (callbackRef.current) {
          callbackRef.current()
        } else if (path) {
          navigate(path)
        }
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [key, path, enabled, navigate])
}
