// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useLocation } from 'react-router'
import { linkedSectionTargets } from '@/lib/linked-section'
import { scrollElementToTop } from '@/lib/scroll-to-element'

/**
 * How long a linked page may take to render the section it points at, and to
 * finish loading the content above it.
 */
const WAIT_FOR_SECTION_MS = 10_000
/** Input that means the reader has started moving around the page. */
const READER_INPUT = ['wheel', 'touchstart', 'pointerdown', 'keydown'] as const
const HIGHLIGHT_MS = 2_000
const HIGHLIGHT_CLASSES = [
  'ring-2',
  'ring-ring',
  'ring-offset-2',
  'ring-offset-background',
  'rounded-lg',
]

/**
 * Brings the element with this DOM id into view and briefly highlights it, so
 * someone who followed a link sees which setting it meant. Opens it first if
 * it is a native disclosure.
 */
export function openSettingsSection(id: string): boolean {
  const element = document.getElementById(id)
  if (!element) return false
  reveal(element)
  return true
}

/**
 * Scrolls only the element's own scroll container (never the app shell or
 * the locked document; see `scroll-to-element.ts`), then highlights it.
 */
function reveal(element: HTMLElement) {
  if (element instanceof HTMLDetailsElement) element.open = true
  scrollElementToTop(element)
  highlight(element)
}

/** Highlights in progress, so a repeat restarts the timer instead of racing it. */
const highlights = new WeakMap<Element, { added: string[]; timer: number }>()

/**
 * Adds the highlight for `HIGHLIGHT_MS`, then removes only the classes it
 * added: a card that is already `rounded-lg` keeps its corners.
 */
function highlight(element: Element) {
  const active = highlights.get(element)
  if (active) window.clearTimeout(active.timer)
  const added =
    active?.added ??
    HIGHLIGHT_CLASSES.filter((name) => !element.classList.contains(name))
  element.classList.add(...added)
  const timer = window.setTimeout(() => {
    element.classList.remove(...added)
    highlights.delete(element)
  }, HIGHLIGHT_MS)
  highlights.set(element, { added, timer })
}

/**
 * Console-wide: when the URL points at a section (`…/settings/security#
 * vulnerability-scanning`, or `?section=build` on the combined project
 * settings pages), scroll to it once it renders. Pages load lazily and fetch
 * before they show their sections, so this waits for the element instead of
 * giving up on the first render. Mounted once, in the app layout.
 *
 * A link can name a setting and the group around it
 * (`?section=build#setting-docker-compose`). The group is shown as soon as it
 * renders, so the page never sits at the top while a card loads; when the
 * setting itself appears it takes over. A setting this service does not have
 * leaves the reader on its group.
 *
 * Sections above the target keep loading after it renders (their queries
 * resolve, skeletons become forms), which pushes it down. Until the reader
 * scrolls, clicks or types, the target is kept where the link put it; from
 * their first input on, the page is theirs and is never scrolled again.
 */
export function useScrollToLinkedSection() {
  const { pathname, search, hash } = useLocation()
  // Only the sections the URL names: another query parameter changing
  // (`?section=agents&tab=runs`) must not scroll the reader back.
  const targets = JSON.stringify(linkedSectionTargets(search, hash))
  useEffect(() => {
    const [primary, ...fallbacks] = JSON.parse(targets) as string[]
    if (!primary) return
    // What the reader is shown: the setting, or its group until it renders.
    let shown: HTMLElement | null = null
    let onPrimary = false
    const follow = () => {
      if (!onPrimary) {
        const target = document.getElementById(primary)
        if (target) {
          onPrimary = true
          shown = target
          reveal(target)
          return
        }
      }
      if (!shown) {
        const group = fallbacks
          .map((id) => document.getElementById(id))
          .find((element) => element !== null)
        if (group) {
          shown = group
          reveal(group)
        }
        return
      }
      // Something rendered: keep the section where the link put it.
      scrollElementToTop(shown)
    }
    const observer = new MutationObserver(follow)
    const timer = window.setTimeout(() => stop(), WAIT_FOR_SECTION_MS)
    const stop = () => {
      observer.disconnect()
      window.clearTimeout(timer)
      for (const type of READER_INPUT)
        window.removeEventListener(type, stop, { capture: true })
    }
    for (const type of READER_INPUT)
      window.addEventListener(type, stop, { capture: true, passive: true })
    follow()
    observer.observe(document.body, { childList: true, subtree: true })
    return stop
  }, [pathname, targets])
}
