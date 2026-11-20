// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'

/**
 * Keyboard access for scrollable containers (WCAG 2.1.1, axe
 * `scrollable-region-focusable`).
 *
 * A container that scrolls but holds nothing focusable cannot be scrolled with
 * the keyboard. The fix is to make the container itself a tab stop -- but only
 * while it actually overflows, otherwise every short table on a page would add
 * a pointless tab stop. `useScrollableRegion` measures overflow with a
 * `ResizeObserver` and returns the props to spread on the container.
 */

type ScrollMetrics = Pick<
  HTMLElement,
  'scrollWidth' | 'clientWidth' | 'scrollHeight' | 'clientHeight'
>

/**
 * Whether content overflows the element on either axis. Sub-pixel rounding can
 * make `scrollWidth` exceed `clientWidth` by one pixel without any scrollbar,
 * so a difference of one pixel or less is ignored.
 */
export function isOverflowing(el: ScrollMetrics): boolean {
  return (
    el.scrollWidth - el.clientWidth > 1 || el.scrollHeight - el.clientHeight > 1
  )
}

export interface ScrollableRegionProps {
  tabIndex?: 0
  role?: 'region'
  'aria-label'?: string
}

/**
 * Props for a scroll container. While it is not scrollable nothing is added,
 * so the element stays out of the tab order. Once it scrolls it becomes a tab
 * stop. A `label` also turns it into a named `region` (`aria-label` is only
 * valid on an element with a role); without one it stays an unnamed, focusable
 * container, because giving every scroller the same generic region name would
 * only trip axe's `landmark-unique` rule.
 */
export function scrollableRegionProps(
  scrollable: boolean,
  label?: string
): ScrollableRegionProps {
  if (!scrollable) return {}
  if (!label) return { tabIndex: 0 }
  return { tabIndex: 0, role: 'region', 'aria-label': label }
}

/**
 * Tracks whether the referenced element overflows. Attach `ref` to the scroll
 * container; `scrollable` flips when its own size or its direct children's
 * size changes. Pass the ref forwarded to the component (if any) as
 * `forwardedRef` so callers keep access to the element.
 */
export function useScrollableRegion<T extends HTMLElement>(
  forwardedRef?: React.Ref<T>
): { ref: React.RefObject<T | null>; scrollable: boolean } {
  const ref = React.useRef<T | null>(null)
  const [scrollable, setScrollable] = React.useState(false)

  React.useImperativeHandle(forwardedRef, () => ref.current as T, [])

  React.useEffect(() => {
    const node = ref.current
    if (!node) return undefined
    const measure = () => setScrollable(isOverflowing(node))
    measure()
    if (typeof ResizeObserver === 'undefined') return undefined
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    for (const child of Array.from(node.children)) observer.observe(child)
    return () => observer.disconnect()
  }, [])

  return { ref, scrollable }
}
