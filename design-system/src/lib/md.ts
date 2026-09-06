// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Markdown slicing for the consolidated guide (`/guide`).
 *
 * The documents in `docs/` stay the single source of truth: the guide
 * imports them with Vite's `?raw` and renders them. Nothing here rewrites
 * prose — it only cuts a document at heading boundaries, reads its headings
 * for the search index, and splits a bullet list into addressable entries.
 *
 * Every cut is by an exact line prefix (`## 4. Tokens`), so a renamed heading
 * fails loudly (an empty section) instead of quietly shifting the boundary.
 */

/** Lines inside a fenced code block are never headings and never bullets. */
function withoutFences(lines: string[]): boolean[] {
  const inFence: boolean[] = []
  let open = false
  for (const line of lines) {
    if (/^\s*```/.test(line)) {
      open = !open
      inFence.push(true)
      continue
    }
    inFence.push(open)
  }
  return inFence
}

/**
 * The text of a document from the line starting with `from` up to (not
 * including) the line starting with `to`. `to` omitted runs to the end.
 * Returns '' when `from` is not found, which renders as an empty section —
 * a visible failure rather than a silent one.
 */
export function slice(md: string, from: string, to?: string): string {
  const lines = md.split('\n')
  const start = lines.findIndex((l) => l.startsWith(from))
  if (start === -1) return ''
  const rest = lines.slice(start)
  const end = to ? rest.findIndex((l, i) => i > 0 && l.startsWith(to)) : -1
  return (end === -1 ? rest : rest.slice(0, end)).join('\n').trimEnd()
}

/** A URL-safe id for a heading or a taste entry. Stable across renders. */
export function slug(text: string): string {
  return text
    .toLowerCase()
    .replace(/[`*_]/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
}

/** Strip the inline markdown a heading may carry, so the id and the search index use words. */
export function plain(text: string): string {
  return text
    .replace(/`([^`]*)`/g, '$1')
    .replace(/\*\*([^*]*)\*\*/g, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .trim()
}

export type Heading = { depth: number; text: string }

/** Every ATX heading in a slice, code fences excluded. */
export function headings(md: string): Heading[] {
  const lines = md.split('\n')
  const fenced = withoutFences(lines)
  const out: Heading[] = []
  lines.forEach((line, i) => {
    if (fenced[i]) return
    const m = /^(#{1,6})\s+(.*)$/.exec(line)
    if (m) out.push({ depth: m[1].length, text: plain(m[2]) })
  })
  return out
}

export type Bullet = { title: string; body: string }

/**
 * Split a top-level `- ` list into one entry per bullet, with the leading
 * `**Bold lead.**` lifted out as the entry's title. Brand §6 (Taste) is
 * written that way, and the guide gives each of those bullets its own
 * anchor so a review can link to one rule.
 */
export function bullets(md: string): Bullet[] {
  const lines = md.split('\n')
  const fenced = withoutFences(lines)
  const chunks: string[][] = []
  lines.forEach((line, i) => {
    if (fenced[i]) {
      chunks[chunks.length - 1]?.push(line)
      return
    }
    if (/^- /.test(line)) chunks.push([line.slice(2)])
    else if (chunks.length && /^\s{2,}\S/.test(line)) chunks[chunks.length - 1].push(line.replace(/^ {2}/, ''))
    else if (chunks.length && line.trim() === '') chunks[chunks.length - 1].push('')
  })
  return chunks.map((chunk) => {
    const body = chunk.join('\n').trim()
    const m = /^\*\*(.+?)\*\*[.:]?\s*/s.exec(body)
    return m
      ? { title: plain(m[1]).replace(/[.:]$/, ''), body: body.slice(m[0].length) }
      : { title: plain(body.split('\n')[0]).slice(0, 70), body }
  })
}

/**
 * The numbered audit items carrying a given marker, with their continuation
 * lines. The UX audit marks deferred work `⏳` and partly-done work `◐`; the
 * guide's "Open questions" section is exactly those two sets.
 */
export function markedItems(md: string, marker: string): string {
  const lines = md.split('\n')
  const out: string[] = []
  let taking = false
  for (const line of lines) {
    const starts = /^\d+\.\s/.test(line)
    if (starts) taking = line.includes(marker)
    else if (line.trim() === '' || !/^\s/.test(line)) taking = taking && line.trim() !== ''
    if (taking) out.push(line)
  }
  return out.join('\n').trim()
}

/** Make a list of ids unique by suffixing repeats, so two bullets named alike still deep-link. */
export function unique(ids: string[]): string[] {
  const seen = new Map<string, number>()
  return ids.map((id) => {
    const n = (seen.get(id) ?? 0) + 1
    seen.set(id, n)
    return n === 1 ? id : `${id}-${n}`
  })
}
