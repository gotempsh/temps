// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Scrolling an element to the top of the view without moving the app shell.
 *
 * `Element.scrollIntoView({ block: 'start' })` scrolls *every* scrollable
 * ancestor until the element is at the top, including `overflow: hidden`
 * ones and the locked document. The console is a fixed-height shell whose
 * pages scroll in an inner container, so for a section near the bottom of a
 * page (one that cannot reach the top of its container) the browser went on to
 * scroll the hidden-overflow frame and the body as well: the whole shell slid
 * up, the header and the top of the page were cut off, a blank band opened
 * below, and nothing the user could do scrolled it back. These helpers scroll
 * only the element's own scroll container, clamped to what it can scroll.
 */

const SCROLLING_OVERFLOW = new Set(['auto', 'scroll', 'overlay'])

/** Computed vertical overflow, falling back to the shorthand's last value. */
function overflowY(element: Element): string {
  const style = getComputedStyle(element)
  return style.overflowY || style.overflow.split(' ').pop() || ''
}

/**
 * The nearest ancestor that scrolls `element` vertically and has something to
 * scroll, or `null` when that is the document. An ancestor that merely clips
 * (`overflow: hidden`) or only scrolls sideways is skipped.
 */
export function scrollContainerOf(element: Element): HTMLElement | null {
  const root = element.ownerDocument
  for (
    let node = element.parentElement;
    node && node !== root.body && node !== root.documentElement;
    node = node.parentElement
  ) {
    if (
      SCROLLING_OVERFLOW.has(overflowY(node)) &&
      node.scrollHeight > node.clientHeight
    )
      return node
  }
  return null
}

/**
 * Where `container` (or the document, when `null`) must be scrolled for
 * `element`'s top, less its `scroll-margin-top`, to sit at the top of it.
 * Not clamped: the browser clamps to what the container can scroll.
 */
export function scrollTopToAlign(
  element: Element,
  container: HTMLElement | null
): number {
  const margin = Number.parseFloat(getComputedStyle(element).scrollMarginTop)
  const elementTop = element.getBoundingClientRect().top
  const viewTop = container
    ? container.getBoundingClientRect().top + container.clientTop
    : 0
  const current = container
    ? container.scrollTop
    : (element.ownerDocument.scrollingElement?.scrollTop ?? 0)
  return Math.max(
    0,
    Math.round(
      elementTop - viewTop + current - (Number.isFinite(margin) ? margin : 0)
    )
  )
}

/**
 * Scrolls `element` to the top of its own scroll container and nothing else.
 * Returns whether anything moved. When the element is not inside a scroll
 * container the document scrolls, unless the app shell has locked it
 * (`overflow: hidden` on the body), in which case nothing is moved.
 */
export function scrollElementToTop(element: Element): boolean {
  const container = scrollContainerOf(element)
  const doc = element.ownerDocument
  const scroller = container ?? doc.scrollingElement
  if (!scroller) return false
  // Already there, or as far as it goes: nothing to do. Called again on every
  // render while a linked page loads, so this must stay cheap.
  const top = Math.min(
    scrollTopToAlign(element, container),
    Math.max(0, scroller.scrollHeight - scroller.clientHeight)
  )
  if (top === Math.round(scroller.scrollTop)) return false
  if (container) {
    container.scrollTo({ top })
    return true
  }
  const view = doc.defaultView
  if (!view || overflowY(doc.body) === 'hidden') return false
  view.scrollTo({ top })
  return true
}
