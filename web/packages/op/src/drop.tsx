// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useLayoutEffect, useState, type ReactNode, type RefObject } from 'react'
import { cn } from './lib/cn'

/**
 * A panel hanging off a control (attention, notifications, a context meter).
 * From sm up it is absolute against the anchor's parent, right-aligned, at most
 * `width`. On a phone a right-anchored panel runs off the left edge, so below sm
 * it is fixed, edge to edge with 0.75rem gutters, clear of the anchor's near
 * edge. The anchor's parent must be `relative`.
 *
 * `side` decides which edge it hangs from, once, in the component: `below` for a
 * header control, `above` for a control in a bottom bar (a phone form decided
 * here, not per screen — a bottom-bar panel opening downwards is off-screen).
 */
export function Drop({ anchor, open, width = 480, side = 'below', role = 'dialog', label, className, children }: { anchor: RefObject<HTMLElement | null>; open: boolean; width?: number; side?: 'below' | 'above'; role?: 'dialog' | 'menu' | 'listbox'; label?: string; className?: string; children: ReactNode }) {
  const [fixed, setFixed] = useState<{ top?: number; bottom?: number } | null>(null)
  useLayoutEffect(() => {
    if (!open) return
    const place = () => {
      const r = anchor.current?.getBoundingClientRect()
      if (!r || window.innerWidth >= 640) { setFixed(null); return }
      setFixed(side === 'above' ? { bottom: Math.max(12, window.innerHeight - r.top + 6) } : { top: r.bottom + 6 })
    }
    place()
    window.addEventListener('resize', place)
    window.addEventListener('scroll', place, true) // fixed below sm: follow the anchor when the page scrolls
    return () => { window.removeEventListener('resize', place); window.removeEventListener('scroll', place, true) }
  }, [open, anchor, side])
  if (!open) return null
  return (
    <div
      role={role} aria-label={label}
      style={fixed ? { ...fixed, position: 'fixed', left: '0.75rem', right: '0.75rem', width: 'auto' } : { width: `min(${width}px, calc(100vw - 2rem))` }}
      className={cn('z-30 border bg-background', !fixed && 'absolute right-0', !fixed && (side === 'above' ? 'bottom-9' : 'top-9'), className)}
    >
      {children}
    </div>
  )
}
