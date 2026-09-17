// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useState, type CSSProperties } from 'react'

/** How a composition layer is filled. Four is the ceiling: a fifth pattern is noise. */
export type InkLayerFill = 'solid' | 'hatch' | 'dot' | 'cross'

export const INK_LAYER_ORDER: readonly InkLayerFill[] = [
  'solid',
  'hatch',
  'dot',
  'cross',
]

/** The CSS/SVG paint for each fill. Ink only — a layer never carries a hue. */
export const INK_FILL: Record<InkLayerFill, string> = {
  solid: 'var(--foreground)',
  hatch: 'url(#op-hatch)',
  dot: 'url(#op-dot)',
  cross: 'url(#op-cross)',
}

/** Opacity that goes with the paint, so the four layers land on ≤3 greys. */
export const INK_FILL_OPACITY: Record<InkLayerFill, number> = {
  solid: 0.8,
  hatch: 1,
  dot: 1,
  cross: 1,
}

/** The word a legend prints beside the swatch, so the pattern is named and not only shown. */
export const INK_FILL_WORD: Record<InkLayerFill, string> = {
  solid: 'solid',
  hatch: 'hatched',
  dot: 'dotted',
  cross: 'cross-hatched',
}

/** State tone, for the one layer in a composition that *is* a state (5xx, error). */
export const INK_TONE: Record<'ok' | 'warn' | 'error', string> = {
  ok: 'var(--success)',
  warn: 'var(--warning)',
  error: 'var(--destructive)',
}

/** Five density steps, the same ladder `CalendarHeatmap` uses. Index 0 is "none". */
export const INK_STEPS = [0.06, 0.22, 0.42, 0.68, 1] as const

/** Which of the five steps a value falls in. `0` when the value is zero: nothing is not a light something. */
export function inkStep(value: number, max: number): 0 | 1 | 2 | 3 | 4 {
  if (!value) return 0
  const n = Math.min(4, 1 + Math.floor((value / Math.max(1e-9, max)) * 3.999))
  return n as 1 | 2 | 3 | 4
}

/**
 * The inline style for a density cell. Ink at an opacity, never a colour ramp.
 *
 * The paint is `--op-ink-wash`, not `--foreground`, because night is not paper
 * inverted (brand §4): on paper more ink is darker than the page, and on night
 * it has to be darker than the ground too, or "more" reads as a highlight and
 * the ramp says the opposite of what it means. The variable is black on both
 * layers; `--op-ink-on-dense` is the colour a number sitting *in* a dense cell
 * takes, which is why a cohort percentage stays readable at every step instead
 * of hitting a 50% grey in the middle of the ladder.
 */
export function inkCell(value: number, max: number): CSSProperties {
  return {
    backgroundColor: 'var(--op-ink-wash, var(--foreground))',
    opacity: INK_STEPS[inkStep(value, max)],
  }
}

/**
 * One focusable region whose `←` `→` step an index and announce it, the way
 * `StatusStrip` does. Returns the props to spread on the region and the
 * current index; `null` means nothing is being read.
 */
export function useReadout(count: number) {
  const [i, setI] = useState<number | null>(null)
  const step = useCallback(
    (d: number) =>
      setI((p) =>
        Math.max(0, Math.min(count - 1, (p ?? (d > 0 ? -1 : count)) + d))
      ),
    [count]
  )
  const regionProps = {
    role: 'group' as const,
    tabIndex: 0,
    onFocus: () => setI((p) => p ?? 0),
    onBlur: () => setI(null),
    onKeyDown: (e: React.KeyboardEvent) => {
      if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
        e.preventDefault()
        step(-1)
      }
      if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
        e.preventDefault()
        step(1)
      }
      if (e.key === 'Home') {
        e.preventDefault()
        setI(0)
      }
      if (e.key === 'End') {
        e.preventDefault()
        setI(count - 1)
      }
    },
    className:
      'outline-none focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
  }
  return { i, setI, regionProps }
}
