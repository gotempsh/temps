// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { viewCatalog } from './catalog'
import {
  ASSISTANT_VIEWS_FILE,
  assistantViewsInstructions,
} from './instructions'
import { VIEW_FENCE } from './prompt'
import { parseViewSpec } from './view-spec'

const instructions = assistantViewsInstructions()

describe('what the assistant is told about views', () => {
  test('the file the server sends is the one the catalog generates', () => {
    // After changing the catalog or its prompt, regenerate it:
    // `bun run scripts/write-assistant-views.ts` in web/.
    expect(readFileSync(ASSISTANT_VIEWS_FILE, 'utf8')).toBe(instructions)
  })

  test('lists every component the console can draw, and no other', () => {
    const listed = [...instructions.matchAll(/^- (\w+): \{/gm)].map(
      (match) => match[1]
    )
    expect(listed).toEqual(Object.keys(viewCatalog.data.components))
  })

  test('forbids made-up values instead of asking for sample data', () => {
    expect(instructions).toContain(
      'Show only values a tool returned in this conversation'
    )
    expect(instructions).toContain('Never invent')
    expect(instructions.toLowerCase()).not.toContain('sample data')
    expect(instructions.toLowerCase()).not.toContain('realistic')
  })

  test('offers nothing a view cannot do', () => {
    for (const absent of ['$state', '$bindState', 'setState', '"on":']) {
      expect(instructions).not.toContain(absent)
    }
  })

  test('stays short: it is read whole in each turn that draws a view', () => {
    expect(instructions.length).toBeLessThan(5800)
  })

  test('is a playbook the assistant reads when a result is to be drawn', () => {
    const header = /^---\n([\s\S]*?)\n---\n/.exec(instructions)?.[1] ?? ''
    expect(header).toContain('name: show-results-as-views')
    expect(header).toContain('contexts: assistant')
    expect(header).toMatch(/^description: .*list of records/m)
  })

  test('its own example is a view the console draws whole', () => {
    const fence = new RegExp('```' + VIEW_FENCE + '\\n([\\s\\S]*?)\\n```').exec(
      instructions
    )
    const view = parseViewSpec(fence?.[1] ?? '')
    expect(view?.dropped).toBe(0)
    expect(Object.keys(view?.spec.elements ?? {})).toEqual([
      'main',
      'totals',
      'running',
      'failing',
      'services',
    ])
  })
})
