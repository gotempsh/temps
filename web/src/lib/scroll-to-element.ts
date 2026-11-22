// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { preferredScrollBehavior } from './scroll-behavior'

/**
 * Scrolling an element into view without moving the app shell.
 *
 * `Element.scrollIntoView` scrolls *every* scrollable ancestor until the
 * element sits where it was asked to, including `overflow: hidden` ones and
 * the locked document. The console is a fixed-height shell whose pages scroll
 * in an inner container, so for an element the inner container cannot bring
 * all the way (a section near the bottom of a page asked to the top or the
 * centre, or a row taller than the view) the browser went on to scroll the
 * hidden-overflow frame and the body as well: the whole shell slid up, the
 * header and the top of the page were cut off, a blank band opened below, and
 * nothing the user could do scrolled it back. These helpers scroll only the
 * element's own scroll container, clamped to what it can scroll.
 */

const SCROLLING_OVERFLOW = new Set(['auto', 'scroll', 'overlay'])

/**
 * Where the element should end up in its scroll container, as in
 * `scrollIntoView`'s `block`: its top at the top, centred, or the least
 * scrolling that makes it visible (none when it already is).
 */
export type ScrollBlock = 'start' | 'center' | 'nearest'

export interface ScrollToElementOptions {
  /** Default `start`. */
  block?: ScrollBlock
  /**
   * Default: the page's own (an instant jump). `smooth` animates, unless the
   * user asked the system for reduced motion, in which case it jumps.
   */
  behavior?: ScrollBehavior
}

/** Computed vertical overflow, falling back to the shorthand's last value. */
function overflowY(element: Element): string {
  const style = getComputedStyle(element)
  return style.overflowY || style.overflow.split(' ').pop() || ''
}

/** A computed length in px, `0` when it is not one. */
function px(value: string): number {
  const parsed = Number.parseFloat(value)
  return Number.isFinite(parsed) ? parsed : 0
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
 * Where `container` (or the document, when `null`) must be scrolled to put
 * `element` where `block` asks, its box grown by its `scroll-margin`. For
 * `nearest` that is where the container already is when the element is
 * visible, or when it covers the whole view. Not clamped to what the
 * container can scroll, beyond never being negative.
 */
export function scrollTopToAlign(
  element: Element,
  container: HTMLElement | null,
  block: ScrollBlock = 'start'
): number {
  const doc = element.ownerDocument
  const style = getComputedStyle(element)
  const box = element.getBoundingClientRect()
  const current = container
    ? container.scrollTop
    : (doc.scrollingElement?.scrollTop ?? 0)
  const viewTop = container
    ? container.getBoundingClientRect().top + container.clientTop
    : 0
  const viewHeight = container
    ? container.clientHeight
    : doc.documentElement.clientHeight
  // The element in the container's content, margins included.
  const top = box.top - viewTop + current - px(style.scrollMarginTop)
  const height =
    box.height + px(style.scrollMarginTop) + px(style.scrollMarginBottom)
  const alignStart = top
  const alignEnd = top + height - viewHeight
  let target: number
  if (block === 'start') target = alignStart
  else if (block === 'center') target = top - (viewHeight - height) / 2
  else {
    const above = top < current
    const below = top + height > current + viewHeight
    // Already visible, or larger than the view and covering all of it.
    if (above === below) return Math.round(current)
    // As `scrollIntoView`: an element that fits is brought in by the edge
    // that was out of view; one that does not keeps the edge that was in.
    const fits = height <= viewHeight
    target = above === fits ? alignStart : alignEnd
  }
  return Math.max(0, Math.round(target))
}

/**
 * Scrolls `element` into view in its own scroll container and nothing else.
 * Returns whether anything moved. When the element is not inside a scroll
 * container the document scrolls, unless the app shell has locked it
 * (`overflow: hidden` on the body), in which case nothing is moved.
 *
 * An element that is not rendered (`hidden`, `display: none`, or inside
 * either) has no box to scroll to, so nothing moves, as with the native
 * call: measured, its empty box reads as the top of the view and would
 * scroll a reader who had moved down back up without showing anything.
 */
export function scrollElementIntoView(
  element: Element,
  { block = 'start', behavior }: ScrollToElementOptions = {}
): boolean {
  if (element.getClientRects().length === 0) return false
  const container = scrollContainerOf(element)
  const doc = element.ownerDocument
  const scroller = container ?? doc.scrollingElement
  if (!scroller) return false
  // Already there, or as far as it goes: nothing to do. Called again on every
  // render while a linked page loads, so this must stay cheap.
  const top = Math.min(
    scrollTopToAlign(element, container, block),
    Math.max(0, scroller.scrollHeight - scroller.clientHeight)
  )
  if (top === Math.round(scroller.scrollTop)) return false
  const options: ScrollToOptions = behavior
    ? {
        top,
        behavior: behavior === 'smooth' ? preferredScrollBehavior() : behavior,
      }
    : { top }
  if (container) {
    container.scrollTo(options)
    return true
  }
  const view = doc.defaultView
  if (!view || overflowY(doc.body) === 'hidden') return false
  view.scrollTo(options)
  return true
}

/**
 * Scrolls `element` to the top of its own scroll container and nothing else;
 * `scrollElementIntoView` with `block: 'start'`.
 */
export function scrollElementToTop(
  element: Element,
  options: Omit<ScrollToElementOptions, 'block'> = {}
): boolean {
  return scrollElementIntoView(element, { ...options, block: 'start' })
}

/**
 * `scrollElementIntoView` for the element with this DOM id. Returns whether
 * anything moved; `false` as well when there is no such element yet.
 */
export function scrollToElementId(
  id: string,
  options?: ScrollToElementOptions
): boolean {
  const element = document.getElementById(id)
  return element ? scrollElementIntoView(element, options) : false
}
