// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'

/** The part of the page the user can actually see, in CSS pixels. */
export interface ViewportFit {
  height: number
  top: number
}

/**
 * Reads the visible area from a `VisualViewport`. A phone keyboard shrinks the
 * visual viewport without resizing the layout viewport, and the browser then
 * scrolls the page to reveal the focused field, pushing the header off screen.
 * Sizing the shell to this instead keeps the header and the field both visible.
 */
export function viewportFit(
  viewport: Pick<VisualViewport, 'height' | 'offsetTop'>
): ViewportFit {
  return {
    height: Math.max(0, Math.round(viewport.height)),
    top: Math.max(0, Math.round(viewport.offsetTop)),
  }
}

const HEIGHT_VAR = '--app-height'
const TOP_VAR = '--app-top'

/**
 * While enabled, publishes the visible height and offset as `--app-height` and
 * `--app-top` on the root element and keeps the layout viewport scrolled to the
 * top. Without `visualViewport` (old browsers) nothing is set, and the
 * variables' CSS fallbacks apply.
 */
export function useVisualViewportFit(enabled: boolean): void {
  useEffect(() => {
    const viewport = window.visualViewport
    if (!enabled || !viewport) return
    const root = document.documentElement
    const apply = () => {
      const fit = viewportFit(viewport)
      root.style.setProperty(HEIGHT_VAR, `${fit.height}px`)
      root.style.setProperty(TOP_VAR, `${fit.top}px`)
      if (window.scrollY !== 0 || window.scrollX !== 0) window.scrollTo(0, 0)
    }
    apply()
    viewport.addEventListener('resize', apply)
    viewport.addEventListener('scroll', apply)
    return () => {
      viewport.removeEventListener('resize', apply)
      viewport.removeEventListener('scroll', apply)
      root.style.removeProperty(HEIGHT_VAR)
      root.style.removeProperty(TOP_VAR)
    }
  }, [enabled])
}
