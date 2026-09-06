#!/usr/bin/env node
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// tokens.mjs — the token layer is data, and this is what keeps it honest.
//
//   node scripts/tokens.mjs check   compare tokens.json against src/op.css, exit 1 on any drift
//   node scripts/tokens.mjs build   print the CSS block tokens.json describes (does not write)
//
// `check` runs in `bun run lint` from design-system/. It is the enforcement:
// today op.css is hand-written and tokens.json mirrors it, so the two can only
// drift for one edit before CI says so. `build` is the next step — once the
// generated block is byte-identical to the hand-written one (it is; that is
// what `check` proves), op.css's token block can be replaced by its output and
// the direction of truth flips. Doing that flip is deliberately NOT this PR.
//
// What `check` compares, per mode (light / dark):
//   1. every custom property declared in the CSS block exists in tokens.json
//   2. every token in tokens.json exists in the CSS block
//   3. the resolved value matches, ignoring whitespace and comments only
// Asymmetry between light and dark is fine and expected — dark redeclares only
// what changes — so the two modes are compared independently, each against its
// own selector.

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const TOKENS = join(root, 'tokens.json')
const CSS = join(root, 'src', 'op.css')

/* ── tokens.json ─────────────────────────────────────────────────────── */

const doc = JSON.parse(readFileSync(TOKENS, 'utf8'))

/** True for a DTCG token node (has `$value`), false for a group. */
const isToken = (v) => v && typeof v === 'object' && !Array.isArray(v) && '$value' in v

/** Walk a group, calling fn(path, token) for every token under it. */
function walk(node, path, fn) {
  for (const [k, v] of Object.entries(node)) {
    if (k.startsWith('$')) continue
    if (isToken(v)) fn([...path, k], v)
    else if (v && typeof v === 'object') walk(v, [...path, k], fn)
  }
}

/** The token at a dotted path, or undefined. */
function at(path) {
  return path.split('.').reduce((n, k) => (n == null ? undefined : n[k]), doc)
}

/** Resolve `{a.b.c}` aliases to a primitive, following chains, refusing cycles. */
function resolve(value, seen = new Set()) {
  if (typeof value !== 'string') return value
  const m = /^\{([^}]+)\}$/.exec(value.trim())
  if (!m) return value
  const path = m[1]
  if (seen.has(path)) throw new Error(`token alias cycle at {${path}}`)
  const target = at(path)
  if (!isToken(target)) throw new Error(`token alias {${path}} does not resolve to a token`)
  return resolve(target.$value, new Set([...seen, path]))
}

/** Canonical form for comparison: collapse whitespace, drop the space after `--x:`. */
const norm = (v) => String(v).replace(/\s+/g, ' ').trim()

/** { name -> { value, description } } for one semantic mode. */
function tokensForMode(mode) {
  const group = doc.semantic?.[mode]
  if (!group) throw new Error(`tokens.json has no semantic.${mode}`)
  const out = new Map()
  walk(group, [], (path, token) => {
    if (path.length !== 1) throw new Error(`semantic.${mode} must be flat, found ${path.join('.')}`)
    out.set(path[0], { value: norm(resolve(token.$value)), description: token.$description ?? '' })
  })
  return out
}

function selectorsForMode(mode) {
  const s = doc.semantic?.[mode]?.$extensions?.['sh.temps.op']?.selectors
  if (!Array.isArray(s) || s.length === 0) throw new Error(`semantic.${mode} declares no $extensions."sh.temps.op".selectors`)
  return s
}

/* ── op.css ──────────────────────────────────────────────────────────── */

const css = readFileSync(CSS, 'utf8')

/** Source with /* … *\/ comments blanked out, offsets preserved. */
function stripComments(src) {
  return src.replace(/\/\*[\s\S]*?\*\//g, (m) => ' '.repeat(m.length))
}

/**
 * The custom properties declared in the first rule whose selector list contains
 * `selector` as a whole comma-separated entry. Returns null when absent.
 */
function blockFor(selector, src) {
  const clean = stripComments(src)
  let from = 0
  for (;;) {
    const open = clean.indexOf('{', from)
    if (open < 0) return null
    const prev = clean.lastIndexOf('}', open - 1)
    const head = clean.slice(prev + 1, open)
    // Skip at-rules and nested contexts: a token block's selector never has `@`.
    const parts = head.split(',').map((s) => s.replace(/\s+/g, ' ').trim())
    if (!head.includes('@') && parts.includes(selector)) {
      let depth = 1, i = open + 1
      for (; i < clean.length && depth > 0; i++) {
        if (clean[i] === '{') depth++
        else if (clean[i] === '}') depth--
      }
      const body = clean.slice(open + 1, i - 1)
      const props = new Map()
      for (const decl of body.split(';')) {
        const m = /^\s*--([A-Za-z0-9_-]+)\s*:\s*([\s\S]+)$/.exec(decl)
        if (m) props.set(m[1], norm(m[2]))
      }
      return props
    }
    from = open + 1
  }
}

/** The first declared selector that exists in op.css, with its properties. */
function cssForMode(mode) {
  for (const selector of selectorsForMode(mode)) {
    const props = blockFor(selector, css)
    if (props) return { selector, props }
  }
  throw new Error(
    `no ${mode} token block in src/op.css. Tried: ${selectorsForMode(mode).join(', ')}.\n` +
      `If the skin class was renamed, update semantic.${mode}.$extensions."sh.temps.op".selectors in tokens.json.`,
  )
}

/* ── check ───────────────────────────────────────────────────────────── */

function check() {
  const problems = []
  for (const mode of ['light', 'dark']) {
    const want = tokensForMode(mode)
    const { selector, props: have } = cssForMode(mode)
    for (const [name, { value }] of want) {
      if (!have.has(name)) {
        problems.push(`${mode}  --${name}\n    tokens.json: ${value}\n    ${selector}: (not declared)`)
      } else if (have.get(name) !== value) {
        problems.push(`${mode}  --${name}\n    tokens.json: ${value}\n    ${selector}: ${have.get(name)}`)
      }
    }
    for (const [name, value] of have) {
      if (!want.has(name)) {
        problems.push(`${mode}  --${name}\n    tokens.json: (not declared)\n    ${selector}: ${value}`)
      }
    }
    if (want.size && have.size) {
      const order = [...want.keys()].filter((n) => have.has(n))
      const cssOrder = [...have.keys()].filter((n) => want.has(n))
      if (order.join() !== cssOrder.join()) {
        problems.push(`${mode}  declaration order differs from tokens.json order (build would reorder the block)`)
      }
    }
  }

  if (problems.length) {
    console.error(`tokens: ${problems.length} difference${problems.length === 1 ? '' : 's'} between tokens.json and src/op.css\n`)
    for (const p of problems) console.error(p + '\n')
    console.error('Fix whichever is wrong. op.css is currently the source of truth; tokens.json must mirror it.')
    process.exit(1)
  }

  const l = tokensForMode('light').size
  const d = tokensForMode('dark').size
  let base = 0
  walk(doc.base, [], () => base++)
  console.log(`tokens: ok — ${base} base, ${l} semantic light, ${d} semantic dark, all matching src/op.css`)
}

/* ── build ───────────────────────────────────────────────────────────── */

function build() {
  const out = []
  out.push('/* Generated from tokens.json by scripts/tokens.mjs build. Do not edit by hand. */')
  for (const mode of ['light', 'dark']) {
    const { selector } = cssForMode(mode)
    out.push('')
    out.push(`${selector} {`)
    for (const [name, { value, description }] of tokensForMode(mode)) {
      if (description) out.push(`  /* ${description} */`)
      out.push(`  --${name}: ${value};`)
    }
    out.push('}')
  }
  console.log(out.join('\n'))
}

const cmd = process.argv[2]
if (cmd === 'check') check()
else if (cmd === 'build') build()
else {
  console.error('usage: node scripts/tokens.mjs check | build')
  process.exit(2)
}
