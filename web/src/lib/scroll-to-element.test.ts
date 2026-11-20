// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { readdirSync, readFileSync } from 'node:fs'
import { join, relative } from 'node:path'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import {
  scrollContainerOf,
  scrollElementIntoView,
  scrollElementToTop,
  scrollToElementId,
  scrollTopToAlign,
} from './scroll-to-element'

const layout = installFakeScrollLayout()

const originalMatchMedia = window.matchMedia
afterEach(() => {
  window.matchMedia = originalMatchMedia
  document.body.innerHTML = ''
  document.body.style.overflow = ''
  document.documentElement.removeAttribute('data-client-height')
  document.documentElement.removeAttribute('data-scroll-height')
})

/** A document taller than the viewport, as on a standalone page. */
function tallDocument() {
  document.documentElement.setAttribute('data-client-height', '800')
  document.documentElement.setAttribute('data-scroll-height', '2000')
}

/**
 * The console shell: a clipping frame (`overflow: hidden`) around the page's
 * scroll container, a 600px window onto 1,500px of settings. The last
 * section starts 200px above the end, so it can never reach the top.
 */
function mountShell() {
  document.body.innerHTML = `
    <div id="frame" data-scroller data-client-height="800" data-scroll-height="1200" style="overflow: hidden">
      <div id="page" data-scroller data-top="100" data-client-height="600" data-scroll-height="1500" style="overflow-y: auto">
        <section id="previews" data-top="500">Previews</section>
        <section id="feature-flags" data-top="1300">Feature flags</section>
      </div>
    </div>`
  const get = (id: string) => document.getElementById(id) as HTMLElement
  return {
    frame: get('frame'),
    page: get('page'),
    previews: get('previews'),
    flags: get('feature-flags'),
  }
}

test('the scroll container is the nearest ancestor that scrolls, not one that clips', () => {
  const { page, flags } = mountShell()
  expect(scrollContainerOf(flags)).toBe(page)
  expect(scrollContainerOf(page)).toBeNull()
})

test('an ancestor that has nothing to scroll is skipped', () => {
  document.body.innerHTML = `
    <div id="outer" data-scroller data-client-height="500" data-scroll-height="900" style="overflow-y: auto">
      <div id="inner" data-scroller data-client-height="300" data-scroll-height="300" style="overflow-y: auto">
        <p id="target">Target</p>
      </div>
    </div>`
  const target = document.getElementById('target') as HTMLElement
  expect(scrollContainerOf(target)?.id).toBe('outer')
})

test('a section that cannot reach the top scrolls its container to the end and leaves the shell alone', () => {
  const { frame, page, flags } = mountShell()
  expect(scrollElementToTop(flags)).toBe(true)
  expect(layout.scrollTopOf(page)).toBe(900)
  expect(layout.scrollTopOf(frame)).toBe(0)
  expect(layout.scrolls.map((scroll) => scroll.target)).toEqual(['page'])
})

test('a section that can reach the top lands exactly at the top', () => {
  const { page, previews } = mountShell()
  scrollElementToTop(previews)
  expect(layout.scrollTopOf(page)).toBe(500)
  expect(previews.getBoundingClientRect().top).toBe(
    page.getBoundingClientRect().top
  )
})

test('the target is measured from where its container is already scrolled', () => {
  const { page, previews } = mountShell()
  page.scrollTop = 400
  expect(scrollTopToAlign(previews, page)).toBe(500)
  expect(scrollElementToTop(previews)).toBe(true)
  // Already in place: no further scroll.
  expect(scrollElementToTop(previews)).toBe(false)
  expect(layout.scrolls).toEqual([{ target: 'page', top: 500 }])
})

test('a section past the end asks only for what the container can scroll', () => {
  const { page, flags } = mountShell()
  scrollElementToTop(flags)
  expect(scrollElementToTop(flags)).toBe(false)
  expect(layout.scrolls).toEqual([{ target: 'page', top: 900 }])
  expect(layout.scrollTopOf(page)).toBe(900)
})

test('the document is never scrolled while the app shell has locked it', () => {
  tallDocument()
  document.body.innerHTML = `<section id="loose" data-top="700">Loose</section>`
  document.body.style.overflow = 'hidden'
  const loose = document.getElementById('loose') as HTMLElement
  expect(scrollElementToTop(loose)).toBe(false)
  expect(layout.scrolls).toEqual([])
})

test('outside the app shell the document scrolls', () => {
  tallDocument()
  document.body.innerHTML = `<section id="loose" data-top="700">Loose</section>`
  const loose = document.getElementById('loose') as HTMLElement
  scrollElementToTop(loose)
  expect(layout.scrolls).toEqual([{ target: 'window', top: 700 }])
})

/** The system's reduced-motion preference, as `matchMedia` reports it. */
function prefersReducedMotion(reduce: boolean) {
  window.matchMedia = ((query: string) => ({
    matches: reduce && query === '(prefers-reduced-motion: reduce)',
  })) as unknown as typeof window.matchMedia
}

test('centring a row near the end scrolls its container as far as it goes and leaves the shell alone', () => {
  const { frame, page, flags } = mountShell()
  flags.setAttribute('data-height', '100')
  expect(scrollElementIntoView(flags, { block: 'center' })).toBe(true)
  expect(layout.scrollTopOf(page)).toBe(900)
  expect(layout.scrollTopOf(frame)).toBe(0)
  expect(layout.scrolls.map((scroll) => scroll.target)).toEqual(['page'])
})

test('centring puts the middle of the element in the middle of its container', () => {
  const { page, previews } = mountShell()
  previews.setAttribute('data-height', '100')
  // 500 + 100 / 2 - 600 / 2
  expect(scrollTopToAlign(previews, page, 'center')).toBe(250)
  scrollElementIntoView(previews, { block: 'center' })
  expect(layout.scrolls).toEqual([{ target: 'page', top: 250 }])
})

test('nearest leaves a visible element where it is', () => {
  const { page, previews } = mountShell()
  previews.setAttribute('data-height', '100')
  page.scrollTop = 200
  expect(scrollElementIntoView(previews, { block: 'nearest' })).toBe(false)
  expect(layout.scrolls).toEqual([])
})

test('nearest brings an element below the view up to the bottom edge', () => {
  const { frame, flags } = mountShell()
  flags.setAttribute('data-height', '100')
  // 1300 + 100 - 600
  expect(scrollElementIntoView(flags, { block: 'nearest' })).toBe(true)
  expect(layout.scrolls).toEqual([{ target: 'page', top: 800 }])
  expect(layout.scrollTopOf(frame)).toBe(0)
})

test('nearest brings an element above the view down to the top edge', () => {
  const { page, previews } = mountShell()
  previews.setAttribute('data-height', '100')
  page.scrollTop = 800
  scrollElementIntoView(previews, { block: 'nearest' })
  expect(layout.scrolls).toEqual([{ target: 'page', top: 500 }])
})

test('nearest shows the top of an element taller than the view that starts below it', () => {
  const { frame, page, previews } = mountShell()
  previews.setAttribute('data-height', '900')
  page.scrollTop = 0
  scrollElementIntoView(previews, { block: 'nearest' })
  expect(layout.scrolls).toEqual([{ target: 'page', top: 500 }])
  expect(layout.scrollTopOf(frame)).toBe(0)
})

test('nearest leaves an element that covers the whole view alone', () => {
  const { page, previews } = mountShell()
  previews.setAttribute('data-height', '900')
  page.scrollTop = 600
  expect(scrollElementIntoView(previews, { block: 'nearest' })).toBe(false)
  expect(layout.scrolls).toEqual([])
})

test('a smooth scroll animates the container only', () => {
  prefersReducedMotion(false)
  const { page, previews } = mountShell()
  scrollElementToTop(previews, { behavior: 'smooth' })
  expect(layout.scrolls).toEqual([
    { target: 'page', top: 500, behavior: 'smooth' },
  ])
  expect(layout.scrollTopOf(page)).toBe(500)
})

test('a smooth scroll jumps instead when the system asks for reduced motion', () => {
  prefersReducedMotion(true)
  const { previews } = mountShell()
  scrollElementIntoView(previews, { behavior: 'smooth' })
  expect(layout.scrolls).toEqual([
    { target: 'page', top: 500, behavior: 'auto' },
  ])
})

test('a hidden element moves nothing, even when the reader has scrolled down', () => {
  const { page, flags } = mountShell()
  page.scrollTop = 400
  for (const hide of [
    () => flags.setAttribute('hidden', ''),
    () => {
      flags.removeAttribute('hidden')
      page.setAttribute('hidden', '')
    },
    () => {
      page.removeAttribute('hidden')
      flags.style.display = 'none'
    },
  ]) {
    hide()
    for (const block of ['start', 'center', 'nearest'] as const)
      expect(scrollElementIntoView(flags, { block })).toBe(false)
  }
  expect(layout.scrolls).toEqual([])
  expect(layout.scrollTopOf(page)).toBe(400)
})

test('scrolling to an id moves nothing until the element exists', () => {
  const { page } = mountShell()
  expect(scrollToElementId('missing')).toBe(false)
  expect(scrollToElementId('previews')).toBe(true)
  expect(layout.scrollTopOf(page)).toBe(500)
})

test('the native scrollIntoView, which moves the shell, is never used', () => {
  const { previews, flags } = mountShell()
  scrollElementToTop(previews)
  scrollElementIntoView(flags, { block: 'center' })
  scrollElementIntoView(flags, { block: 'nearest' })
  expect(layout.intoView).toEqual([])
})

/**
 * Call sites still allowed to use the native `scrollIntoView`, each because
 * it cannot move the app shell. Anything else under the shell goes through
 * `scrollElementIntoView`.
 */
const NATIVE_SCROLL_INTO_VIEW_ALLOWED: Record<string, string> = {
  // `block: 'nearest'` on one ~32px option inside the suggestion list's own
  // `max-h-60` scroller: the browser stops as soon as the option is visible,
  // and the page's scroll container lies inside the shell's box, so the shell
  // never has to move.
  'components/observability/LogQueryInput.tsx':
    'nearest, on a small option in its own listbox',
}

/** Every production source file under `web/src`. */
function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name)
    if (entry.isDirectory())
      return entry.name === 'node_modules' || entry.name === 'client'
        ? []
        : sourceFiles(path)
    return /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)
      ? [path]
      : []
  })
}

test('no console code calls the native scrollIntoView, which drags the shell along', () => {
  const root = join(import.meta.dir, '..')
  const calls = sourceFiles(root)
    .filter((path) =>
      /\.scrollIntoView\s*(\?\.)?\s*\(/.test(readFileSync(path, 'utf8'))
    )
    .map((path) => relative(root, path))
    .sort()
  expect(calls).toEqual(Object.keys(NATIVE_SCROLL_INTO_VIEW_ALLOWED).sort())
})
