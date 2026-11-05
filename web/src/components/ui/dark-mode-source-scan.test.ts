// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import path from 'node:path'

/**
 * Source-level guard for colours written for the light theme only. Theme
 * tokens (`bg-primary`, `border-border`, `text-muted-foreground`) follow the
 * active theme; a hard-coded tint does not. In dark mode a pastel badge or
 * alert border glares, its dark text sits on a dark surface, and a
 * `gray-950/10` hairline is black on black. Each class below must carry a
 * `dark:` counterpart for the same property in the same class string.
 */

const SRC = path.resolve(import.meta.dir, '../..')

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

const HUES =
  'red|green|blue|yellow|amber|orange|purple|indigo|emerald|teal|cyan|sky|pink|rose|violet|lime|fuchsia'

/** A light-only class and the property its dark counterpart must set. */
const LIGHT_ONLY: { pattern: RegExp; property: string }[] = [
  {
    pattern: new RegExp(`(?<![\\w:/-])border-(?:${HUES})-200(?![\\w/-])`),
    property: 'border',
  },
  {
    pattern: new RegExp(`(?<![\\w:/-])bg-(?:${HUES})-(?:50|100)(?![\\w/-])`),
    property: 'bg',
  },
  {
    pattern: new RegExp(
      `(?<![\\w:/-])text-(?:${HUES})-(?:700|800|900)(?![\\w/-])`
    ),
    property: 'text',
  },
  {
    pattern: /(?<![\w:/-])border-gray-950\/\d+(?![\w-])/,
    property: 'border',
  },
  {
    pattern: /(?<![\w:/-])divide-gray-950\/\d+(?![\w-])/,
    property: 'divide',
  },
  { pattern: /(?<![\w:/-])bg-gray-950\/\d+(?![\w-])/, property: 'bg' },
]

const STRING_LITERAL = /'[^'\n]*'|"[^"\n]*"|`[^`]*`/g

/**
 * A colour value, so `dark:text-xs` or `dark:border-0` (a size or a width)
 * never passes for a dark colour: a palette shade, black/white, a theme token
 * or an arbitrary colour (`[#f87171]`, `[oklch(...)]`, `[var(--x)]`), never an
 * arbitrary size or image such as `[22px]` or `[url(...)]`.
 */
const COLOUR =
  `(?:(?:(?:${HUES}|gray|slate|zinc|neutral|stone)-\\d{2,3}|white|black|transparent|current|` +
  '(?:primary|secondary|muted|accent|destructive|success|warning|foreground|' +
  'background|card|popover|border|input|ring|sidebar|chart-\\d)(?:-[a-z]+)*)' +
  '(?![\\w-])|\\[(?:#[0-9a-fA-F]{3,8}|(?:rgba?|hsla?|hwb|oklch|oklab|lab|lch|color|color-mix|var)\\())'

/** Light-only classes in `classes` that have no `dark:` counterpart. */
function lightOnlyClasses(classes: string): string[] {
  const found: string[] = []
  for (const { pattern, property } of LIGHT_ONLY) {
    const match = classes.match(pattern)
    if (!match) continue
    // The light classes above colour every side, so a one-side dark colour
    // (`dark:border-b-white/5`) leaves the other sides light: only an
    // unsided counterpart counts.
    const counterpart = new RegExp(`(?<![\\w-])dark:${property}-${COLOUR}`)
    if (!counterpart.test(classes)) found.push(match[0])
  }
  return found
}

describe('lightOnlyClasses', () => {
  test('flags a tint with no dark counterpart', () => {
    expect(
      lightOnlyClasses('bg-green-50 text-green-700 border-green-200')
    ).toEqual(['border-green-200', 'bg-green-50', 'text-green-700'])
    expect(lightOnlyClasses('rounded border border-gray-950/10')).toEqual([
      'border-gray-950/10',
    ])
  })

  test('accepts a tint paired with its dark counterpart', () => {
    expect(
      lightOnlyClasses(
        'border-blue-200 dark:border-blue-900 bg-blue-50/50 dark:bg-blue-950/20'
      )
    ).toEqual([])
    expect(lightOnlyClasses('border-gray-950/10 dark:border-white/10')).toEqual(
      []
    )
  })

  test('rejects a dark: utility that sets no colour', () => {
    expect(lightOnlyClasses('text-green-700 dark:text-xs')).toEqual([
      'text-green-700',
    ])
    expect(lightOnlyClasses('border-green-200 dark:border-0')).toEqual([
      'border-green-200',
    ])
    expect(lightOnlyClasses('border-green-200 dark:border-t')).toEqual([
      'border-green-200',
    ])
    expect(lightOnlyClasses('bg-green-50 dark:bg-none')).toEqual([
      'bg-green-50',
    ])
    expect(lightOnlyClasses('text-green-700 dark:text-[22px]')).toEqual([
      'text-green-700',
    ])
    expect(lightOnlyClasses('bg-green-50 dark:bg-[url(/x.png)]')).toEqual([
      'bg-green-50',
    ])
  })

  test('rejects a one-side dark border for an all-sides light border', () => {
    expect(
      lightOnlyClasses('border border-gray-950/5 dark:border-b-white/5')
    ).toEqual(['border-gray-950/5'])
    expect(
      lightOnlyClasses('border-green-200 dark:border-t-green-900')
    ).toEqual(['border-green-200'])
  })

  test('accepts token and arbitrary dark colours', () => {
    expect(lightOnlyClasses('bg-green-50 dark:bg-muted')).toEqual([])
    expect(
      lightOnlyClasses('text-green-700 dark:text-muted-foreground')
    ).toEqual([])
    expect(lightOnlyClasses('text-red-700 dark:text-[#f87171]')).toEqual([])
    expect(
      lightOnlyClasses('bg-green-50 dark:bg-[oklch(0.3_0.1_150)]')
    ).toEqual([])
  })

  test('ignores translucent fills and hover-only shades', () => {
    expect(lightOnlyClasses('bg-blue-50/50 hover:text-blue-700')).toEqual([])
  })
})

describe('dark mode', () => {
  test('hard-coded light tints always carry a dark: counterpart', () => {
    const offenders: string[] = []
    for (const file of sourceFiles(SRC)) {
      const src = readFileSync(file, 'utf8')
      for (const literal of src.matchAll(STRING_LITERAL)) {
        const classes = lightOnlyClasses(literal[0])
        if (classes.length === 0) continue
        const line = src.slice(0, literal.index).split('\n').length
        offenders.push(
          `${path.relative(SRC, file)}:${line} ${classes.join(', ')}`
        )
      }
    }
    expect(offenders).toEqual([])
  })
})
