// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import path from 'node:path'

/**
 * Source-level guards for the axe-core rules that kept coming back across the
 * console (#1443): `aria-prohibited-attr`, `aria-toggle-field-name`,
 * `scrollable-region-focusable` and `color-contrast`. Like
 * `icon-button-names.test.ts` this is a scan rather than a render test, so it
 * covers every page at once; each rule only flags an unambiguous shape.
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

/** Index just past the `>` closing the opening tag, skipping `{...}` and strings. */
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
      return i + 1
    }
  }
  return -1
}

function lineOf(src: string, index: number): number {
  return src.slice(0, index).split('\n').length
}

interface Tag {
  name: string
  text: string
  index: number
}

function* tags(src: string, names: string): Generator<Tag> {
  for (const match of src.matchAll(new RegExp(`<(${names})\\b`, 'g'))) {
    const index = match.index ?? 0
    const end = openingTagEnd(src, index + match[0].length)
    if (end < 0) continue
    yield { name: match[1], text: src.slice(index, end), index }
  }
}

function scan(check: (file: string, src: string) => string[]): string[] {
  const offenders: string[] = []
  for (const file of sourceFiles(SRC)) {
    for (const hit of check(file, readFileSync(file, 'utf8'))) {
      offenders.push(`${path.relative(SRC, file)}:${hit}`)
    }
  }
  return offenders
}

describe('aria-prohibited-attr', () => {
  // Elements with no ARIA role of their own (generic, paragraph, code,
  // strong, ...). `aria-label`/`aria-labelledby` on them is ignored by
  // assistive technology and axe reports it as serious.
  const ROLELESS = 'div|span|p|strong|em|b|i|small|code|pre|dl'
  const ROLELESS_COMPONENTS = 'Badge|Skeleton'

  function roleless(src: string): string[] {
    const found: string[] = []
    for (const tag of tags(src, `${ROLELESS}|${ROLELESS_COMPONENTS}`)) {
      if (!/\baria-(label|labelledby)=/.test(tag.text)) continue
      if (/\brole=/.test(tag.text) || /\{\.\.\./.test(tag.text)) continue
      found.push(
        `${lineOf(src, tag.index)}: <${tag.name}> has aria-label without a role`
      )
    }
    return found
  }

  test('the scan recognises a role-less element with a label', () => {
    expect(
      roleless('<span aria-label="Online" className="dot" />')
    ).toHaveLength(1)
    expect(roleless('<span role="img" aria-label="Online" />')).toHaveLength(0)
    expect(roleless('<button aria-label="Close" />')).toHaveLength(0)
  })

  test('no div/span/p/dl/pre carries aria-label without a role', () => {
    expect(scan((_file, src) => roleless(src))).toEqual([])
  })
})

describe('toggle controls have an accessible name', () => {
  // Switch, Checkbox and radio items render role="switch"/"checkbox"/"radio"
  // buttons with no text, so without a label they have no name at all.
  function unnamed(src: string): string[] {
    const found: string[] = []
    for (const tag of tags(src, 'Switch|Checkbox|RadioGroupItem')) {
      if (
        /\baria-label=|\baria-labelledby=|\{\.\.\.[A-Za-z_.]+\}/.test(tag.text)
      ) {
        continue
      }
      const before = src.slice(Math.max(0, tag.index - 80), tag.index)
      if (/<FormControl>\s*$/.test(before)) continue
      const id = /\bid=(?:"([^"]+)"|\{([^}]+)\})/.exec(tag.text)
      if (id) {
        const value = id[1] ?? id[2]
        const pattern = new RegExp(
          `htmlFor=(?:"${value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}"|\\{${value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\})`
        )
        if (pattern.test(src)) continue
      }
      // Wrapped by a <label> element: the control is named by its text.
      const window = src.slice(Math.max(0, tag.index - 600), tag.index)
      const open = Math.max(
        window.lastIndexOf('<label'),
        window.lastIndexOf('<Label')
      )
      const close = Math.max(
        window.lastIndexOf('</label'),
        window.lastIndexOf('</Label')
      )
      if (open > close) continue
      found.push(
        `${lineOf(src, tag.index)}: <${tag.name}> has no aria-label and no matching htmlFor`
      )
    }
    return found
  }

  test('the scan recognises an unnamed switch', () => {
    expect(
      unnamed('<Switch checked={on} onCheckedChange={set} />')
    ).toHaveLength(1)
    expect(
      unnamed('<Switch aria-label="Enabled" checked={on} />')
    ).toHaveLength(0)
    expect(
      unnamed(
        '<Label htmlFor="x">Enabled</Label><Switch id="x" checked={on} />'
      )
    ).toHaveLength(0)
  })

  test('every Switch, Checkbox and RadioGroupItem is named', () => {
    const offenders = scan((file, src) =>
      file.includes(path.join('components', 'ui') + path.sep)
        ? []
        : unnamed(src)
    )
    expect(offenders).toEqual([])
  })
})

describe('scrollable-region-focusable', () => {
  // A <pre> that scrolls and holds only text cannot be scrolled by keyboard
  // unless it is focusable itself.
  function scrollingPre(src: string): string[] {
    const found: string[] = []
    for (const tag of tags(src, 'pre')) {
      if (!/overflow-(x-|y-)?(auto|scroll)/.test(tag.text)) continue
      if (/\btabIndex=/.test(tag.text)) continue
      found.push(`${lineOf(src, tag.index)}: scrolling <pre> without tabIndex`)
    }
    return found
  }

  test('the scan recognises a scrolling pre without tabIndex', () => {
    expect(
      scrollingPre('<pre className="max-h-48 overflow-auto">x</pre>')
    ).toHaveLength(1)
    expect(
      scrollingPre(
        '<pre tabIndex={0} className="max-h-48 overflow-auto">x</pre>'
      )
    ).toHaveLength(0)
  })

  test('every scrolling <pre> is keyboard focusable', () => {
    expect(scan((_file, src) => scrollingPre(src))).toEqual([])
  })

  // Text-only elements that scroll hold nothing focusable either, so they
  // need the same treatment as a <pre>. `<code overflow-x-auto>` and
  // `<HighlightedCode className="overflow-x-auto">` are the shapes that
  // slipped through; `ScrollRegion` is the fix (it is a tab stop only while
  // it actually overflows).
  function scrollingText(src: string): string[] {
    const found: string[] = []
    for (const tag of tags(src, 'code|samp|kbd|HighlightedCode|p|span')) {
      if (!/overflow-(x-|y-)?(auto|scroll)/.test(tag.text)) continue
      if (/\btabIndex=/.test(tag.text)) continue
      found.push(
        `${lineOf(src, tag.index)}: scrolling <${tag.name}> without tabIndex; wrap it in <ScrollRegion>`
      )
    }
    return found
  }

  test('the scan recognises scrolling inline code without tabIndex', () => {
    expect(
      scrollingText('<code className="block overflow-x-auto">ls</code>')
    ).toHaveLength(1)
    expect(
      scrollingText(
        '<HighlightedCode className="overflow-x-auto" code={c} language="bash" />'
      )
    ).toHaveLength(1)
    expect(
      scrollingText(
        '<ScrollRegion><code className="block">ls</code></ScrollRegion>'
      )
    ).toHaveLength(0)
    expect(
      scrollingText('<code tabIndex={0} className="overflow-x-auto">ls</code>')
    ).toHaveLength(0)
  })

  test('no scrolling code, kbd, p or span is left unreachable by keyboard', () => {
    expect(scan((_file, src) => scrollingText(src))).toEqual([])
  })
})

describe('label-title-only', () => {
  // `title` is not an accessible name for a text field: axe reports
  // `label-title-only` (serious) because it is only exposed as a tooltip.
  // A placeholder is not enough either, so a bare Input needs aria-label,
  // aria-labelledby or a <Label htmlFor>.
  function titleOnly(src: string): string[] {
    const found: string[] = []
    for (const tag of tags(src, 'Input|input|Textarea|textarea')) {
      if (!/\btitle=/.test(tag.text)) continue
      if (/\baria-label=|\baria-labelledby=|\{\.\.\./.test(tag.text)) continue
      if (/\bid=/.test(tag.text)) continue
      if (/\btype="(hidden|checkbox|radio|submit|button)"/.test(tag.text)) {
        continue
      }
      found.push(
        `${lineOf(src, tag.index)}: <${tag.name}> is named only by title`
      )
    }
    return found
  }

  test('the scan recognises a title-only input', () => {
    expect(
      titleOnly('<Input value={v} title="Working directory" />')
    ).toHaveLength(1)
    expect(
      titleOnly(
        '<Input value={v} aria-label="Working directory" title="Working directory" />'
      )
    ).toHaveLength(0)
  })

  test('no text field relies on title alone for its name', () => {
    expect(scan((_file, src) => titleOnly(src))).toEqual([])
  })
})

describe('color-contrast', () => {
  // Names imported from lucide-react / react-icons render SVG graphics; their
  // colour is a non-text contrast question (3:1), not axe's text rule.
  function iconNames(src: string): Set<string> {
    const names = new Set<string>()
    for (const imp of src.matchAll(
      /import\s*\{([^}]*)\}\s*from\s*'(?:lucide-react|react-icons\/[a-z0-9]+)'/g
    )) {
      for (const part of imp[1].split(',')) {
        const name = part.split(' as ').pop()?.trim()
        if (name) names.add(name)
      }
    }
    return names
  }

  function insideIconTag(
    src: string,
    icons: Set<string>,
    index: number
  ): boolean {
    for (const name of icons) {
      const open = src.lastIndexOf(`<${name}`, index)
      if (open < 0) continue
      const end = openingTagEnd(src, open + name.length + 1)
      if (end > index) return true
    }
    return false
  }

  function literalAround(src: string, index: number): string {
    const lineStart = src.lastIndexOf('\n', index) + 1
    const lineEnd = src.indexOf('\n', index)
    const line = src.slice(lineStart, lineEnd < 0 ? src.length : lineEnd)
    const at = index - lineStart
    let best: [number, number] | null = null
    for (const quote of ['"', "'", '`']) {
      const a = line.lastIndexOf(quote, at)
      const b = line.indexOf(quote, at)
      if (a >= 0 && b >= 0 && (!best || a > best[0])) best = [a, b]
    }
    return best ? line.slice(best[0] + 1, best[1]) : line
  }

  // Surfaces that are always dark, where a light text shade is correct.
  const ALWAYS_DARK =
    /bg-(black|(zinc|neutral|gray|slate|stone)-[89]\d\d)|hover:text-(white|zinc-100|zinc-50|gray-100|neutral-100)/

  // Shades below 4.5:1 on a white or tinted surface in the light theme.
  // `dark:` / `hover:` variants are not matched (the lookbehind rejects `:`).
  const LOW_CONTRAST = new RegExp(
    '(?<![\\w:\\[/!-])(' +
      'text-muted-foreground/(30|40|50|60|70|80)|' +
      'text-destructive/(70|80)|' +
      'text-(green|emerald|amber|yellow|orange|red|rose|pink|sky|cyan|teal|lime)-(400|500|600)|' +
      'text-(blue|purple|violet|indigo|gray|slate|neutral|zinc|stone)-(400|500)' +
      ')(?![\\w/\\[-])',
    'g'
  )

  function lowContrast(src: string): string[] {
    const icons = iconNames(src)
    const found: string[] = []
    for (const match of src.matchAll(LOW_CONTRAST)) {
      const index = match.index ?? 0
      if (insideIconTag(src, icons, index)) continue
      const literal = literalAround(src, index)
      if (ALWAYS_DARK.test(literal)) continue
      // Hover/focus tweaks in a literal that also has a light-aware colour
      // are fine; a bare low-contrast text colour is not.
      found.push(`${lineOf(src, index)}: ${match[1]}`)
    }
    return found
  }

  test('the scan recognises low-contrast text colours', () => {
    expect(
      lowContrast('<span className="text-muted-foreground/60">x</span>')
    ).toHaveLength(1)
    expect(
      lowContrast('<span className="text-green-500">ok</span>')
    ).toHaveLength(1)
    expect(
      lowContrast(
        '<span className="text-green-700 dark:text-green-500">ok</span>'
      )
    ).toHaveLength(0)
    expect(
      lowContrast('<span className="text-muted-foreground">x</span>')
    ).toHaveLength(0)
  })

  test('icons are exempt from the text-contrast scan', () => {
    expect(
      lowContrast(
        'import { Check } from \'lucide-react\'\n<Check className="size-4 text-green-500" />'
      )
    ).toHaveLength(0)
  })

  test('no text uses a shade or opacity that is below 4.5:1 in the light theme', () => {
    expect(scan((_file, src) => lowContrast(src))).toEqual([])
  })
})
