// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import githubDark from 'shiki/themes/github-dark.mjs'
import { CodeBlock } from './code-block'
import { withReadableDarkComments } from './code-block-theme'

installBrowserWindow()
afterEach(cleanup)

// Long unwrapped lines scroll horizontally; axe `scrollable-region-focusable`
// requires such a scroller to be reachable from the keyboard.
test('the code scroller is not a tab stop while the code fits', () => {
  const view = render(<CodeBlock code="echo hi" language="bash" />)
  const pre = view.container.querySelector('pre')
  const scroller = pre?.parentElement
  expect(scroller).toBeTruthy()
  expect(scroller?.hasAttribute('tabindex')).toBe(false)
})

test('the code scroller becomes a tab stop once the code overflows', () => {
  // happy-dom does no layout, so report an overflowing box for every element.
  const widths = { scrollWidth: 900, clientWidth: 300 }
  const originals = Object.entries(widths).map(
    ([key]) =>
      [
        key,
        Object.getOwnPropertyDescriptor(HTMLElement.prototype, key),
      ] as const
  )
  for (const [key, value] of Object.entries(widths)) {
    Object.defineProperty(HTMLElement.prototype, key, {
      configurable: true,
      get: () => value,
    })
  }
  try {
    const view = render(
      <CodeBlock code={'x'.repeat(400)} language="bash" disableWrapToggle />
    )
    const scroller = view.container.querySelector('pre')?.parentElement
    expect(scroller?.getAttribute('tabindex')).toBe('0')
  } finally {
    for (const [key, descriptor] of originals) {
      if (descriptor)
        Object.defineProperty(HTMLElement.prototype, key, descriptor)
      else Reflect.deleteProperty(HTMLElement.prototype, key)
    }
  }
})

// github-dark's comment grey measured 3.45:1 on the dark code panel; axe
// `color-contrast` needs 4.5:1.
test('dark comments are lightened and other token colours are untouched', () => {
  const colours = (theme: typeof githubDark) =>
    (theme.tokenColors ?? []).map((rule) =>
      rule.settings?.foreground?.toLowerCase()
    )
  const before = colours(githubDark)
  const after = colours(withReadableDarkComments(githubDark))

  expect(before).toContain('#6a737d')
  expect(after).not.toContain('#6a737d')
  expect(after).toContain('#8b949e')
  expect(after).toHaveLength(before.length)
  before.forEach((colour, index) => {
    if (colour !== '#6a737d') expect(after[index]).toBe(colour)
  })
})
