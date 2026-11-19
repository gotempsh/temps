// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import {
  scrollContainerOf,
  scrollElementToTop,
  scrollTopToAlign,
} from './scroll-to-element'

const layout = installFakeScrollLayout()

afterEach(() => {
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
