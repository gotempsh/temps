// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect } from 'bun:test'
import { createElement, type ReactNode } from 'react'
import type { installFakeScrollLayout } from './fake-scroll-layout'

type FakeScrollLayout = ReturnType<typeof installFakeScrollLayout>

/**
 * The console's layout, for scroll tests run under `installFakeScrollLayout`:
 * the shell's column (`#shell`), which clips (`overflow: hidden`) so script
 * can scroll it but the reader never can, around the page's scroll container
 * (`#console`), 100px below the shell's top: a 700px window onto 2,000px of
 * page. Anything placed below 1,300px cannot reach the top of the console.
 */
export const CONSOLE = { top: 100, height: 700, content: 2000 } as const

/** As far as `#console` can scroll. */
export const CONSOLE_MAX_SCROLL = CONSOLE.content - CONSOLE.height

/** Where a test puts its target: near the end of the page, out of view. */
export const NEAR_THE_END = { top: 1900, height: 60 } as const

export function ConsoleShell({ children }: { children: ReactNode }) {
  const page = createElement(
    'div',
    {
      id: 'console',
      'data-scroller': '',
      'data-top': CONSOLE.top,
      'data-client-height': CONSOLE.height,
      'data-scroll-height': CONSOLE.content,
      style: { overflowY: 'auto' },
    },
    children
  )
  return createElement(
    'div',
    {
      id: 'shell',
      'data-scroller': '',
      'data-client-height': 800,
      'data-scroll-height': 1400,
      style: { overflow: 'hidden' },
    },
    page
  )
}

/**
 * The page's scroll container moved to `top`, and nothing else did: the
 * shell stayed put and `scrollIntoView`, which drags it along, was never
 * called.
 */
export function expectOnlyConsoleScrolled(
  layout: FakeScrollLayout,
  top: number
) {
  expect(layout.intoView).toEqual([])
  expect(layout.scrolls.length).toBeGreaterThan(0)
  expect(new Set(layout.scrolls.map((scroll) => scroll.target))).toEqual(
    new Set(['console'])
  )
  const shell = document.getElementById('shell')
  const page = document.getElementById('console')
  expect(shell && layout.scrollTopOf(shell)).toBe(0)
  expect(page && layout.scrollTopOf(page)).toBe(top)
}
