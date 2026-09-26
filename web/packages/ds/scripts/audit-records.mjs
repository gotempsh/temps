#!/usr/bin/env node
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Fails if a scanned file hand-rolls a raw hex/oklch color or a bare px/ms
// literal instead of a semantic token/Tailwind class. Honour-system for
// everything else RULES.md asks for (see docs/design-system-handoff.md's
// machine-checked-vs-honour-system table) — this script only catches the
// mechanically detectable subset.
//
//   node scripts/audit-records.mjs --dir src

import { readFileSync, readdirSync, statSync } from 'node:fs'
import path from 'node:path'

const SCAN_EXTENSIONS = new Set(['.ts', '.tsx', '.css'])
const EXCLUDED_FILENAMES = new Set(['tokens.css', 'tokens.json'])
const EXCLUDED_DIRS = new Set(['node_modules', 'dist', '.turbo'])
const IGNORE_MARKER = 'audit-ignore'

const HEX_COLOR = /#(?:[0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})\b/g
const OKLCH = /\boklch\(/gi
const RAW_PX_OR_MS = /(?<![\w.-])\d+(?:\.\d+)?(?:px|ms)(?![\w-])/g

function parseArgs(argv) {
  const dirIndex = argv.indexOf('--dir')
  const dir = dirIndex >= 0 ? argv[dirIndex + 1] : null
  if (!dir) {
    console.error('usage: node scripts/audit-records.mjs --dir <path>')
    process.exit(1)
  }
  return { dir }
}

function walk(dir, files = []) {
  for (const entry of readdirSync(dir)) {
    if (EXCLUDED_DIRS.has(entry)) continue
    const full = path.join(dir, entry)
    const stat = statSync(full)
    if (stat.isDirectory()) {
      walk(full, files)
    } else if (SCAN_EXTENSIONS.has(path.extname(entry)) && !EXCLUDED_FILENAMES.has(entry)) {
      files.push(full)
    }
  }
  return files
}

function scanFile(file) {
  const content = readFileSync(file, 'utf8')
  const problems = []
  content.split('\n').forEach((line, i) => {
    if (line.includes(IGNORE_MARKER)) return
    for (const [name, pattern] of [
      ['hex color', HEX_COLOR],
      ['oklch() literal', OKLCH],
      ['raw px/ms literal', RAW_PX_OR_MS],
    ]) {
      pattern.lastIndex = 0
      const match = pattern.exec(line)
      if (match) {
        problems.push(`${file}:${i + 1}: ${name} "${match[0]}" — use a token/Tailwind class instead`)
      }
    }
  })
  return problems
}

function main() {
  const { dir } = parseArgs(process.argv.slice(2))
  const root = path.resolve(process.cwd(), dir)
  const files = walk(root)
  const problems = files.flatMap(scanFile)

  if (problems.length > 0) {
    console.error(`audit-records failed (${problems.length} problem${problems.length === 1 ? '' : 's'}):\n`)
    for (const p of problems) console.error(`  - ${p}`)
    console.error(
      `\nIf this is a deliberate, reviewed exception, add "// ${IGNORE_MARKER}" on the same line.`,
    )
    process.exit(1)
  }
  console.log(`audit-records passed — scanned ${files.length} file(s) under ${dir}`)
}

main()
