// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import path from 'node:path'

/**
 * Regression guard for WCAG 4.1.2 (axe `button-name`): a button whose only
 * content is an icon has no text, so it needs an `aria-label` (or
 * `aria-labelledby` / `title`) or a screen reader announces just "button".
 *
 * This is a source scan rather than a render test so it covers every page at
 * once. It only flags the unambiguous shape -- `<Button ...><Icon /></Button>`
 * with nothing else inside -- so it has no false positives to suppress.
 */

const SRC = path.resolve(import.meta.dir, '../..')
const OPEN = /<(Button|button)\b/g
const ONLY_ICON = /^<([A-Z][A-Za-z0-9]*)\b[^<>]*\/>$/s
const NAMED = /aria-label|aria-labelledby|\btitle=/

function* sourceFiles(dir: string): Generator<string> {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry)
    if (statSync(full).isDirectory()) {
      // Generated client code has no JSX buttons.
      if (full.endsWith(path.join('api', 'client'))) continue
      yield* sourceFiles(full)
    } else if (entry.endsWith('.tsx') && !entry.endsWith('.test.tsx')) {
      yield full
    }
  }
}

/** Index just past the `>` closing the opening tag, skipping `{...}` and strings. */
function openingTagEnd(
  src: string,
  from: number
): { end: number; selfClosing: boolean } {
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
      return { end: i + 1, selfClosing: src[i - 1] === '/' }
    }
  }
  return { end: -1, selfClosing: false }
}

function unnamedIconButtons(src: string): string[] {
  const found: string[] = []
  for (const match of src.matchAll(OPEN)) {
    const tag = match[1]
    const { end, selfClosing } = openingTagEnd(
      src,
      (match.index ?? 0) + match[0].length
    )
    if (end < 0 || selfClosing) continue
    const close = src.indexOf(`</${tag}>`, end)
    if (close < 0) continue
    const inner = src.slice(end, close).trim()
    const icon = ONLY_ICON.exec(inner)
    if (!icon) continue
    const opening = src.slice(match.index ?? 0, end)
    if (NAMED.test(opening)) continue
    const line = src.slice(0, match.index ?? 0).split('\n').length
    found.push(`${line}: <${tag}> with only <${icon[1]} />`)
  }
  return found
}

test('the scan recognises an unnamed icon-only button', () => {
  expect(
    unnamedIconButtons(
      '<Button variant="ghost" size="icon" onClick={() => go(-1)}>\n  <ArrowLeft className="h-4 w-4" />\n</Button>'
    )
  ).toHaveLength(1)
  expect(
    unnamedIconButtons(
      '<Button aria-label="Back" onClick={() => go(-1)}><ArrowLeft /></Button>'
    )
  ).toHaveLength(0)
  // Visible text or an sr-only label already names the button.
  expect(unnamedIconButtons('<Button><Plus /> Add</Button>')).toHaveLength(0)
  expect(
    unnamedIconButtons(
      '<Button><X /><span className="sr-only">Close</span></Button>'
    )
  ).toHaveLength(0)
})

test('every icon-only button in the console has an accessible name', () => {
  const offenders: string[] = []
  for (const file of sourceFiles(SRC)) {
    for (const hit of unnamedIconButtons(readFileSync(file, 'utf8'))) {
      offenders.push(`${path.relative(SRC, file)}:${hit}`)
    }
  }
  expect(offenders).toEqual([])
})
