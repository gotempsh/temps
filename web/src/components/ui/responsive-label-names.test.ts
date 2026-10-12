// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import path from 'node:path'

/**
 * Regression guard for WCAG 4.1.2 (axe `button-name`) on narrow screens.
 *
 * `<span className="hidden sm:inline">Label</span>` next to an icon is
 * `display: none` below `sm`, so the button has no name at all on a phone: the
 * label is removed from the accessibility tree together with the pixels. Use
 * `max-sm:sr-only` instead, which hides the text visually but keeps it as the
 * accessible name.
 *
 * Source scan, like `icon-button-names.test.ts`: it only flags a button whose
 * sole text is such a span and that has no `aria-label`/`title` of its own.
 */

const SRC = path.resolve(import.meta.dir, '../..')
const OPEN = /<(Button|button)\b/g
const NAMED = /aria-label|aria-labelledby|\btitle=/
const HIDDEN_LABEL =
  /<span\b[^>]*\bhidden sm:inline\b(?!-)[^>]*>[\s\S]*?<\/span>/g

function* sourceFiles(dir: string): Generator<string> {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry)
    if (statSync(full).isDirectory()) {
      if (full.endsWith(path.join('api', 'client'))) continue
      yield* sourceFiles(full)
    } else if (entry.endsWith('.tsx') && !entry.endsWith('.test.tsx')) {
      yield full
    }
  }
}

function openingTagEnd(src: string, from: number): number {
  let depth = 0
  let quote: string | null = null
  for (let i = from; i < src.length; i++) {
    const c = src[i]
    if (quote) {
      if (c === quote) quote = null
    } else if (depth === 0 && (c === '"' || c === "'" || c === '`')) {
      quote = c
    } else if (c === '{') {
      depth++
    } else if (c === '}') {
      depth--
    } else if (c === '>' && depth === 0) {
      return src[i - 1] === '/' ? -1 : i + 1
    }
  }
  return -1
}

function buttonsLabelledOnlyByHiddenText(src: string): string[] {
  const found: string[] = []
  for (const match of src.matchAll(OPEN)) {
    const start = match.index ?? 0
    const end = openingTagEnd(src, start + match[0].length)
    if (end < 0) continue
    const close = src.indexOf(`</${match[1]}>`, end)
    if (close < 0) continue
    const inner = src.slice(end, close)
    if (!HIDDEN_LABEL.test(inner)) continue
    HIDDEN_LABEL.lastIndex = 0
    if (NAMED.test(src.slice(start, end))) continue
    const rest = inner
      .replace(HIDDEN_LABEL, '')
      .replace(/<[^>]*>/g, '')
      .trim()
    HIDDEN_LABEL.lastIndex = 0
    // Any other text or `{expression}` outside the hidden span names it.
    if (/[A-Za-z0-9{]/.test(rest)) continue
    found.push(`${src.slice(0, start).split('\n').length}`)
  }
  return found
}

test('the scan recognises a button named only by a mobile-hidden span', () => {
  expect(
    buttonsLabelledOnlyByHiddenText(
      '<Button><Plus /><span className="hidden sm:inline">Add</span></Button>'
    )
  ).toHaveLength(1)
  // Visually hidden text on mobile is still the accessible name.
  expect(
    buttonsLabelledOnlyByHiddenText(
      '<Button><Plus /><span className="max-sm:sr-only">Add</span></Button>'
    )
  ).toHaveLength(0)
  expect(
    buttonsLabelledOnlyByHiddenText(
      '<Button aria-label="Add"><Plus /><span className="hidden sm:inline">Add</span></Button>'
    )
  ).toHaveLength(0)
  expect(
    buttonsLabelledOnlyByHiddenText(
      '<Button><Plus />Add <span className="hidden sm:inline">project</span></Button>'
    )
  ).toHaveLength(0)
})

test('no console button loses its only label below the sm breakpoint', () => {
  const offenders: string[] = []
  for (const file of sourceFiles(SRC)) {
    for (const line of buttonsLabelledOnlyByHiddenText(
      readFileSync(file, 'utf8')
    )) {
      offenders.push(`${path.relative(SRC, file)}:${line}`)
    }
  }
  expect(offenders).toEqual([])
})
