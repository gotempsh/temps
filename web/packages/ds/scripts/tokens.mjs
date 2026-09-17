#!/usr/bin/env node
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Keeps tokens.json, src/globals.css (the app's actual token source) and this
// package's generated src/tokens.css from drifting apart.
//
//   node scripts/tokens.mjs build   # (re)generate src/tokens.css from tokens.json
//   node scripts/tokens.mjs check   # fail if tokens.json disagrees with either file

import { readFileSync, writeFileSync, existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const here = path.dirname(fileURLToPath(import.meta.url))
const pkgRoot = path.resolve(here, '..')
const tokensPath = path.join(pkgRoot, 'tokens.json')
const cssOutPath = path.join(pkgRoot, 'src', 'tokens.css')
const globalsCssPath = path.resolve(pkgRoot, '../../src/globals.css')

const ROOT_CLASS = '.tds'

function loadTokens() {
  return JSON.parse(readFileSync(tokensPath, 'utf8'))
}

/** Resolves a `{a.b.c}` alias against the full token tree; returns the raw $value otherwise. */
function resolveValue(tokens, raw, seen = new Set()) {
  const match = /^\{([^}]+)\}$/.exec(String(raw).trim())
  if (!match) return raw
  const refPath = match[1]
  if (seen.has(refPath)) {
    throw new Error(`tokens.json: circular alias at ${refPath}`)
  }
  seen.add(refPath)
  const node = refPath
    .split('.')
    .reduce((acc, key) => (acc == null ? acc : acc[key]), tokens)
  if (!node || typeof node.$value === 'undefined') {
    throw new Error(`tokens.json: unresolved alias {${refPath}}`)
  }
  return resolveValue(tokens, node.$value, seen)
}

/** Flattens one semantic layer (light/dark) into [{ name, cssVar, value, group }]. */
function flattenLayer(tokens, layer) {
  const out = []
  for (const [group, entries] of Object.entries(layer)) {
    for (const [name, token] of Object.entries(entries)) {
      const cssVar = token.$extensions?.css
      if (!cssVar) continue
      out.push({
        group,
        name,
        cssVar,
        value: resolveValue(tokens, token.$value),
      })
    }
  }
  return out
}

/** Parses `selector { --a: b; ... }` (flat, no nested rules) into a Map<varName, value>. */
function parseCssVarBlock(css, selectorRegex) {
  const match = selectorRegex.exec(css)
  if (!match) return new Map()
  const start = match.index + match[0].length
  const end = css.indexOf('}', start)
  const body = css.slice(start, end).replace(/\/\*[\s\S]*?\*\//g, '')
  const vars = new Map()
  for (const decl of body.split(';')) {
    const m = /^\s*(--[a-z0-9-]+)\s*:\s*(.+?)\s*$/is.exec(decl)
    if (!m) continue
    vars.set(m[1], m[2].replace(/\s+/g, ' ').trim())
  }
  return vars
}

function normalize(value) {
  return value.replace(/\s+/g, ' ').trim()
}

function buildCss(tokens) {
  const light = flattenLayer(tokens, tokens.semantic.light)
  const dark = flattenLayer(tokens, tokens.semantic.dark)
  const radius = tokens.base.radius
  const font = tokens.base.font
  const tracking = tokens.base.tracking
  const spacing = tokens.base.spacing

  const lightLines = light.map((t) => `  ${t.cssVar}: ${t.value};`).join('\n')
  const darkLines = dark.map((t) => `  ${t.cssVar}: ${t.value};`).join('\n')

  return `/* SPDX-FileCopyrightText: 2024-2026 Temps Contributors */
/* SPDX-License-Identifier: MIT OR Apache-2.0 */

/* GENERATED FILE — do not hand-edit. Run \`node scripts/tokens.mjs build\` from
   web/packages/ds after changing tokens.json. \`bun run lint\` (tokens:check)
   fails the build if this file drifts from tokens.json or from the app's own
   web/src/globals.css.

   Scoped to ${ROOT_CLASS}, never :root — this file must never fight the host
   app's own :root theme. Consumers opt in by adding the ${ROOT_CLASS} class
   to a subtree (the sandbox app's <body> does this; see design-system/). */

${ROOT_CLASS} {
${lightLines}
  --radius: ${radius.default.$value};
  --radius-sm: ${radius.sm.$value};
  --radius-md: ${radius.md.$value};
  --radius-lg: ${radius.lg.$value};
  --radius-xl: ${radius.xl.$value};
  --spacing: ${spacing.unit.$value};
  --tracking-normal: ${tracking.normal.$value};
  --letter-spacing: ${tracking['letter-spacing'].$value};
  --font-sans: ${font.sans.$value};
  --font-serif: ${font.serif.$value};
  --font-mono: ${font.mono.$value};
}

${ROOT_CLASS}.dark {
${darkLines}
}
`
}

function checkAgainstGlobalsCss(tokens) {
  if (!existsSync(globalsCssPath)) {
    return [`web/src/globals.css not found at ${globalsCssPath}`]
  }
  const css = readFileSync(globalsCssPath, 'utf8')
  const rootVars = parseCssVarBlock(css, /:root\s*\{/g)
  const darkVars = parseCssVarBlock(css, /\.dark\s*\{/g)

  const problems = []
  for (const [layerName, layer, cssVars] of [
    ['light', tokens.semantic.light, rootVars],
    ['dark', tokens.semantic.dark, darkVars],
  ]) {
    for (const t of flattenLayer(tokens, layer)) {
      const cssValue = cssVars.get(t.cssVar)
      if (cssValue === undefined) {
        problems.push(
          `semantic.${layerName}.${t.group}.${t.name}: globals.css has no ${t.cssVar}`,
        )
        continue
      }
      if (normalize(cssValue) !== normalize(t.value)) {
        problems.push(
          `semantic.${layerName}.${t.group}.${t.name} (${t.cssVar}): tokens.json says "${t.value}", globals.css says "${cssValue}"`,
        )
      }
    }
  }
  return problems
}

function main() {
  const cmd = process.argv[2]
  const tokens = loadTokens()

  if (cmd === 'build') {
    writeFileSync(cssOutPath, buildCss(tokens))
    console.log(`wrote ${path.relative(pkgRoot, cssOutPath)}`)
    return
  }

  if (cmd === 'check') {
    const problems = []

    const expectedCss = buildCss(tokens)
    if (!existsSync(cssOutPath)) {
      problems.push(`${path.relative(pkgRoot, cssOutPath)} does not exist — run 'node scripts/tokens.mjs build'`)
    } else if (readFileSync(cssOutPath, 'utf8') !== expectedCss) {
      problems.push(
        `${path.relative(pkgRoot, cssOutPath)} is stale — run 'node scripts/tokens.mjs build' and commit the result`,
      )
    }

    problems.push(...checkAgainstGlobalsCss(tokens))

    if (problems.length > 0) {
      console.error(`tokens:check failed (${problems.length} problem${problems.length === 1 ? '' : 's'}):\n`)
      for (const p of problems) console.error(`  - ${p}`)
      process.exit(1)
    }
    console.log('tokens:check passed — tokens.json, src/tokens.css and web/src/globals.css agree')
    return
  }

  console.error('usage: node scripts/tokens.mjs <build|check>')
  process.exit(1)
}

main()
