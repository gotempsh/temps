// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Record-page recipe audit. Static, runs in `bun run lint` and in CI.
// Fails when a record page breaks a rule the templates cannot see at runtime:
//   1. <Lede …> without `facts=`            (a lede carries the four to six facts)
//   2. <Detail … lede={…}> without `meta=`  (the meta places the record)
//   3. a KeyValue row keyed "project · environment" / "message id" / "id"   (a fact appears once: those live in meta or lede)
//   4. a literal word in a `meta=` that comes back as a fact `k:`/`v:`      (a fact appears once)
//   5. <Detail status=…> wrapping <Columns> with no `lede=`                 (a record page has a lede)
// Rules 4 and 5 are heuristic and literal-only: a fact assembled from an
// expression is invisible to them. Runtime warnings in Lede/Detail catch the
// same in the browser during dev.
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

const dir = 'src/sections'
const files = readdirSync(dir).filter((f) => f.endsWith('.tsx')).map((f) => join(dir, f))

// Words that carry no record identity, so repeating them says nothing.
const COMMON = new Set([
  'true', 'false', 'null', 'undefined', 'span', 'div', 'className', 'text', 'font', 'mono', 'muted',
  'foreground', 'background', 'border', 'flex', 'items', 'center', 'gap', 'this', 'that', 'with',
  'from', 'into', 'over', 'when', 'what', 'then', 'than', 'none', 'never', 'always', 'more', 'less',
  'only', 'also', 'here', 'them', 'they', 'been', 'have', 'does', 'both', 'each', 'same', 'other',
  'every', 'about', 'after', 'before', 'still', 'there', 'which', 'while', 'would', 'yet', 'and',
  'the', 'for', 'not', 'but', 'was', 'are', 'its', 'one', 'two',
])

/** Text of every string literal in an expression, `${…}` interpolations dropped. */
function literalText(expr) {
  let out = ''
  for (let i = 0; i < expr.length; i++) {
    const c = expr[i]
    if (c === "'" || c === '"') {
      let j = i + 1
      for (; j < expr.length && expr[j] !== c; j++) if (expr[j] === '\\') j++
      out += ' ' + expr.slice(i + 1, j) + ' '
      i = j
    } else if (c === '`') {
      let j = i + 1, depth = 0
      for (; j < expr.length; j++) {
        if (expr[j] === '\\') { j++; continue }
        if (depth === 0 && expr[j] === '$' && expr[j + 1] === '{') { depth = 1; j++; continue }
        if (depth > 0) { if (expr[j] === '{') depth++; else if (expr[j] === '}') depth--; continue }
        if (expr[j] === '`') break
        out += expr[j]
      }
      out = out + ' '
      i = j
    }
  }
  return out
}

function words(text) {
  return (text.toLowerCase().match(/[a-z][a-z0-9_-]{3,}/g) ?? []).filter((w) => !COMMON.has(w))
}

/** The attribute source of a JSX opening tag starting at `start`, plus the index just past `>`. */
function openTag(src, start) {
  let i = src.indexOf(' ', start), depth = 0, quote = ''
  if (i < 0) return null
  for (; i < src.length; i++) {
    const c = src[i]
    if (quote) { if (c === '\\') i++; else if (c === quote) quote = ''; continue }
    if (c === "'" || c === '"' || c === '`') { quote = c; continue }
    if (c === '{') depth++
    else if (c === '}') depth--
    else if (c === '>' && depth <= 0) return { attrs: src.slice(start, i), end: i + 1 }
  }
  return null
}

/** The value expression of `name=` in an attribute source, or null. */
function attr(attrs, name) {
  const m = new RegExp(`\\b${name}=`).exec(attrs)
  if (!m) return null
  const at = m.index + m[0].length
  if (attrs[at] === '"' || attrs[at] === "'") {
    const q = attrs[at], end = attrs.indexOf(q, at + 1)
    return attrs.slice(at, end < 0 ? attrs.length : end + 1)
  }
  if (attrs[at] !== '{') return null
  let depth = 0, quote = ''
  for (let i = at; i < attrs.length; i++) {
    const c = attrs[i]
    if (quote) { if (c === '\\') i++; else if (c === quote) quote = ''; continue }
    if (c === "'" || c === '"' || c === '`') { quote = c; continue }
    if (c === '{') depth++
    else if (c === '}' && --depth === 0) return attrs.slice(at + 1, i)
  }
  return null
}

const lineOf = (src, index) => src.slice(0, index).split('\n').length

/** Top-level function/const declarations, so a rule can stay inside one component. */
function components(src) {
  const starts = [...src.matchAll(/^(?:export\s+)?(?:function|const|class)\s/gm)].map((m) => m.index)
  if (!starts.length) return [{ at: 0, text: src }]
  return starts.map((at, i) => ({ at, text: src.slice(at, starts[i + 1] ?? src.length) }))
}

const problems = []
for (const file of files) {
  // Comments are blanked (same length, so line numbers hold) so a rule cannot trip on `<Columns>` inside a JSX or line comment.
  const src = readFileSync(file, 'utf8').replace(/\/\*[\s\S]*?\*\/|(^|[^:])\/\/[^\n]*/g, (m, pre) => (pre ?? '') + ' '.repeat(m.length - (pre ?? '').length))
  const lines = src.split('\n')
  lines.forEach((line, i) => {
    const n = i + 1
    for (const m of line.matchAll(/<Lede\s+([^>]*\bstate=[^>]*)>/g)) { // real JSX only, not the `<Lede state word>` doc string
      if (!/\bfacts=/.test(m[1])) problems.push(`${file}:${n} <Lede> without facts=`)
    }
    for (const m of line.matchAll(/<Detail\b([^>]*)>/g)) {
      if (/\blede=/.test(m[1]) && !/\bmeta=/.test(m[1])) problems.push(`${file}:${n} <Detail lede=…> without meta=`)
    }
    if (/\{\s*k:\s*'(project · environment|message id|id)'/.test(line)) problems.push(`${file}:${n} KeyValue repeats a fact that belongs in the meta or the lede`)
  })

  // Rule 4: a literal word in a Detail meta that comes back as a fact key or
  // value of the same component. Scoped to the component so two records in one
  // file do not accuse each other.
  for (const comp of components(src)) {
    if (!/<Lede\b[^>]*\bfacts=/.test(comp.text)) continue
    const factWords = new Set()
    for (const m of comp.text.matchAll(/\b[kv]:\s*(['"`])((?:\\.|(?!\1)[\s\S])*?)\1/g)) {
      for (const w of words(literalText(m[1] + m[2] + m[1]))) factWords.add(w)
    }
    for (const m of comp.text.matchAll(/<Detail\b/g)) {
      const tag = openTag(comp.text, m.index)
      if (!tag) continue
      const meta = attr(tag.attrs, 'meta')
      if (!meta) continue
      const repeats = [...new Set(words(literalText(meta)))].filter((w) => factWords.has(w))
      if (repeats.length) problems.push(`${file}:${lineOf(src, comp.at + m.index)} <Detail meta=…> repeats a Lede fact: ${repeats.join(', ')} (a fact appears once)`)
    }
  }

  // Rule 5: a record page (Detail with a status line, laid out in Columns) has a Lede.
  for (const m of src.matchAll(/<Detail\b/g)) {
    const tag = openTag(src, m.index)
    if (!tag) continue
    if (!/\bstatus=/.test(tag.attrs) || /\blede=/.test(tag.attrs)) continue
    const close = src.indexOf('</Detail>', tag.end)
    const body = src.slice(tag.end, close < 0 ? src.length : close)
    if (/<Columns\b/.test(body)) problems.push(`${file}:${lineOf(src, m.index)} <Detail status=…> with <Columns> and no lede= (a record page has a Lede)`)
  }
}
if (problems.length) {
  console.error('record recipe audit failed:\n' + problems.map((p) => '  ' + p).join('\n') + '\n\nsee docs/design-system-handoff.md §7 "Record page checklist"')
  process.exit(1)
}
console.log(`record recipe audit: ${files.length} files, no problems`)
