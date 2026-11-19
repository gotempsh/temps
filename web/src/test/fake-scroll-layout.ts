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
 * - `data-scroller` plus `data-client-height` / `data-scroll-height`: a box
 *   with that visible height and content height. Its `scrollTop` is clamped
 *   to what it can scroll, like a browser's.
 *
 * `scrolls` records every `scrollTo` on an element or the window, in order.
 */
export function installFakeScrollLayout() {
  const scrolls: { target: string; top: number }[] = []
  const positions = new WeakMap<Element, number>()
  const restore: (() => void)[] = []

  const number = (element: Element, name: string) =>
    Number(element.getAttribute(name) ?? 0)
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

  beforeEach(() => {
    scrolls.length = 0
    const proto = Element.prototype
    override(proto, 'getBoundingClientRect', {
      value(this: Element) {
        const y = top(this)
        return {
          top: y,
          y,
          bottom: y,
          left: 0,
          right: 0,
          x: 0,
          width: 0,
          height: 0,
        }
      },
    })
    override(proto, 'scrollHeight', {
      get(this: Element) {
        return number(this, 'data-scroll-height')
      },
    })
    // happy-dom defines this one on `HTMLElement`, below `Element`.
    override(HTMLElement.prototype, 'clientHeight', {
      get(this: Element) {
        return number(this, 'data-client-height')
      },
    })
    override(proto, 'scrollTop', {
      get(this: Element) {
        return positions.get(this) ?? 0
      },
      set(this: Element, value: number) {
        positions.set(this, Math.min(Math.max(0, value), maxScroll(this)))
      },
    })
    override(proto, 'scrollTo', {
      value(this: Element, options: ScrollToOptions) {
        scrolls.push({ target: this.id, top: options.top ?? 0 })
        ;(this as HTMLElement).scrollTop = options.top ?? 0
      },
    })
    override(window, 'scrollTo', {
      value(options: ScrollToOptions) {
        scrolls.push({ target: 'window', top: options.top ?? 0 })
        document.documentElement.scrollTop = options.top ?? 0
      },
    })
  })
  afterEach(() => {
    while (restore.length) restore.pop()?.()
  })

  return {
    scrolls,
    scrollTopOf: (element: Element) => positions.get(element) ?? 0,
  }
}
