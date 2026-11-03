// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useLayoutEffect, useState } from 'react'

/**
 * The live width of an element, for layouts that follow the space they were
 * given rather than the window (a sidebar takes room the viewport still counts).
 * Null until measured. Resize observations arrive before paint, so a layout
 * chosen from this does not flash the wrong arrangement first.
 *
 * A callback ref, so an element that mounts after loading is still measured.
 */
export function useElementWidth<T extends HTMLElement>() {
  const [element, ref] = useState<T | null>(null)
  const [width, setWidth] = useState<number | null>(null)

  useLayoutEffect(() => {
    if (!element || typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(entry.contentRect.width)
    })
    observer.observe(element)
    return () => observer.disconnect()
  }, [element])

  return [ref, width] as const
}
