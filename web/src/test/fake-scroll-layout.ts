// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach } from 'bun:test'

/**
 * A minimal vertical layout for happy-dom, which has none, so scrolling code
 * can be tested against real-looking geometry:
 *
 * - `data-top="N"`: the element's top, in px, within the content of its
 *   closest `[data-scroller]` ancestor (or of the viewport when there is none).
 *   A scroller is positioned the same way within its own scroller.
 * - `data-height="N"`: the element's own height, in px (default 0).
 * - An element that is `hidden`, or inside a `hidden` one, or `display:
 *   none`, has no client rects, as in a browser.
 * - `data-scroller` plus `data-client-height` / `data-scroll-height`: a box
 *   with that visible height and content height. Its `scrollTop` is clamped
 *   to what it can scroll, like a browser's.
 *
 * Elements a component renders itself, which a test cannot put attributes
 * on, are placed with `place(selector, { top, height })` instead.
 *
 * `scrolls` records every `scrollTo` on an element or the window, in order,
 * with its `behavior` when one was given. `intoView` records the id of every
 * element `scrollIntoView` was called on, the call that moves the shell.
 */
export function installFakeScrollLayout() {
  const scrolls: { target: string; top: number; behavior?: ScrollBehavior }[] =
    []
  const intoView: string[] = []
  const positions = new WeakMap<Element, number>()
  const placed = new Map<string, { top: number; height?: number }>()
  const restore: (() => void)[] = []

  const placedValue = (element: Element, name: string) => {
    let place: { top: number; height?: number } | undefined
    for (const [selector, value] of placed)
      if (element.matches(selector)) place = value
    if (name === 'data-top') return place?.top
    if (name === 'data-height') return place?.height
    return undefined
  }
  const number = (element: Element, name: string) =>
    placedValue(element, name) ?? Number(element.getAttribute(name) ?? 0)
  const maxScroll = (element: Element) =>
    Math.max(
      0,
      number(element, 'data-scroll-height') -
        number(element, 'data-client-height')
    )
  const scrollerOf = (element: Element) =>
    element.parentElement?.closest('[data-scroller]') ?? null
  const top = (element: Element): number => {
    const scroller = scrollerOf(element)
    const origin = scroller ? top(scroller) - (positions.get(scroller) ?? 0) : 0
    return origin + number(element, 'data-top')
  }

  const override = (
    proto: object,
    name: string,
    descriptor: PropertyDescriptor
  ) => {
    const previous = Object.getOwnPropertyDescriptor(proto, name)
    Object.defineProperty(proto, name, { configurable: true, ...descriptor })
    restore.push(() => {
      if (previous) Object.defineProperty(proto, name, previous)
      else Reflect.deleteProperty(proto, name)
    })
  }

  // On `HTMLElement` as well as `Element`: happy-dom defines some of these
  // (`clientHeight`) on `HTMLElement`, and a test elsewhere in the run that
  // patched one there and restored it by assignment leaves an own copy that
  // would hide an `Element`-level fake.
  const overrideElement = (name: string, descriptor: PropertyDescriptor) => {
    override(Element.prototype, name, descriptor)
    override(HTMLElement.prototype, name, descriptor)
  }

  const record = (target: string, options: ScrollToOptions) =>
    scrolls.push({
      target,
      top: options.top ?? 0,
      ...(options.behavior ? { behavior: options.behavior } : {}),
    })

  beforeEach(() => {
    scrolls.length = 0
    intoView.length = 0
    placed.clear()
    overrideElement('getBoundingClientRect', {
      value(this: Element) {
        const y = top(this)
        const height = number(this, 'data-height')
        return {
          top: y,
          y,
          bottom: y + height,
          left: 0,
          right: 0,
          x: 0,
          width: 0,
          height,
        }
      },
    })
    // No boxes for an element that is not rendered, as in a browser.
    overrideElement('getClientRects', {
      value(this: Element) {
        const hidden =
          this.closest('[hidden]') !== null ||
          getComputedStyle(this).display === 'none'
        return hidden ? [] : [this.getBoundingClientRect()]
      },
    })
    overrideElement('scrollHeight', {
      get(this: Element) {
        return number(this, 'data-scroll-height')
      },
    })
    overrideElement('clientHeight', {
      get(this: Element) {
        return number(this, 'data-client-height')
      },
    })
    overrideElement('scrollTop', {
      get(this: Element) {
        return positions.get(this) ?? 0
      },
      set(this: Element, value: number) {
        positions.set(this, Math.min(Math.max(0, value), maxScroll(this)))
      },
    })
    overrideElement('scrollTo', {
      value(this: Element, options: ScrollToOptions) {
        record(this.id, options)
        ;(this as HTMLElement).scrollTop = options.top ?? 0
      },
    })
    overrideElement('scrollIntoView', {
      value(this: Element) {
        intoView.push(this.id)
      },
    })
    override(window, 'scrollTo', {
      value(options: ScrollToOptions) {
        record('window', options)
        document.documentElement.scrollTop = options.top ?? 0
      },
    })
  })
  afterEach(() => {
    while (restore.length) restore.pop()?.()
  })

  return {
    scrolls,
    intoView,
    scrollTopOf: (element: Element) => positions.get(element) ?? 0,
    place: (selector: string, place: { top: number; height?: number }) => {
      placed.set(selector, place)
    },
  }
}
